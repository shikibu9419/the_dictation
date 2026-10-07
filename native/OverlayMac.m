#import <AppKit/AppKit.h>
#import <ApplicationServices/ApplicationServices.h>
#import <QuartzCore/QuartzCore.h>
#import <CoreImage/CoreImage.h>

static NSWindow *panel;
static NSView *gpuiView;
static NSVisualEffectView *glass;
static CALayer *neon, *halo;
static CAGradientLayer *rim, *bloom;

static CAGradientLayer *makeRim(CGFloat width, float opacity) {
    CAGradientLayer *gradient = [CAGradientLayer layer];
    gradient.colors = @[(id)[NSColor colorWithSRGBRed:0.25 green:0.85 blue:1 alpha:1].CGColor,
        (id)[NSColor colorWithSRGBRed:0.6 green:0.4 blue:1 alpha:1].CGColor,
        (id)[NSColor colorWithSRGBRed:1 green:0.35 blue:0.65 alpha:1].CGColor,
        (id)[NSColor colorWithSRGBRed:1 green:0.75 blue:0.3 alpha:1].CGColor,
        (id)[NSColor colorWithSRGBRed:0.4 green:1 blue:0.75 alpha:1].CGColor];
    gradient.startPoint = CGPointMake(0, 0);
    gradient.endPoint = CGPointMake(1, 1);
    CAShapeLayer *mask = [CAShapeLayer layer];
    mask.fillColor = NSColor.clearColor.CGColor;
    mask.strokeColor = NSColor.whiteColor.CGColor;
    mask.lineWidth = width;
    gradient.mask = mask;
    gradient.opacity = opacity;
    return gradient;
}
static void layoutRim(void) {
    CGRect bounds = panel.contentView.bounds;
    neon.frame = bounds;
    halo.frame = bounds;
    CGPathRef path = CGPathCreateWithRoundedRect(CGRectInset(bounds, 10, 10), 18, 18, NULL);
    for (CAGradientLayer *gradient in @[rim, bloom]) {
        gradient.frame = bounds;
        gradient.mask.frame = bounds;
        ((CAShapeLayer *)gradient.mask).path = path;
    }
    CGPathRelease(path);
}
static void pinContents(CALayer *layer) {
    if ([layer isKindOfClass:CAMetalLayer.class]) layer.contentsGravity = kCAGravityTopLeft;
    for (CALayer *child in layer.sublayers) pinContents(child);
}
bool index_reduce_motion(void) { return NSWorkspace.sharedWorkspace.accessibilityDisplayShouldReduceMotion; }
bool index_panel_visible(void) { return panel.isVisible && panel.alphaValue > 0; }
static id outsideMonitor, localMonitor, inputSourceMonitor;
static NSStatusItem *status;
static void (*actionCallback)(int);

@interface IndexMenuActions : NSObject
@end
@implementation IndexMenuActions
- (void)action:(NSMenuItem *)sender { if (actionCallback) actionCallback((int)sender.tag); }
- (void)spaceChanged:(NSNotification *)note {
    if (panel.isVisible) [panel orderFrontRegardless];
}
@end
static IndexMenuActions *menuActions;

void index_panel_setup(void *view, void (*callback)(int)) {
    gpuiView = (__bridge NSView *)view;
    panel = gpuiView.window;
    actionCallback = callback;
    dispatch_async(dispatch_get_main_queue(), ^{
    [NSApp setActivationPolicy:NSApplicationActivationPolicyAccessory];
    // Stay above application windows but below system IME candidate panels.
    panel.level = NSFloatingWindowLevel;
    panel.collectionBehavior = NSWindowCollectionBehaviorCanJoinAllSpaces |
        NSWindowCollectionBehaviorCanJoinAllApplications |
        NSWindowCollectionBehaviorFullScreenAuxiliary |
        NSWindowCollectionBehaviorStationary |
        NSWindowCollectionBehaviorIgnoresCycle;
    panel.hidesOnDeactivate = NO;
    panel.opaque = NO;
    panel.backgroundColor = NSColor.clearColor;
    panel.hasShadow = NO;
    panel.titleVisibility = NSWindowTitleHidden;
    panel.titlebarAppearsTransparent = YES;
    panel.contentView.wantsLayer = YES;
    panel.contentView.layer.masksToBounds = NO;
    glass = [[NSVisualEffectView alloc] initWithFrame:NSInsetRect(panel.contentView.bounds, 10, 10)];
    glass.autoresizingMask = NSViewWidthSizable | NSViewHeightSizable;
    glass.material = NSVisualEffectMaterialHUDWindow;
    glass.alphaValue = 0.85;
    glass.blendingMode = NSVisualEffectBlendingModeBehindWindow;
    glass.state = NSVisualEffectStateActive;
    glass.wantsLayer = YES;
    glass.layer.cornerRadius = 18;
    glass.layer.masksToBounds = YES;
    CIFilter *frost = [CIFilter filterWithName:@"CIGaussianBlur"];
    [frost setValue:@10 forKey:kCIInputRadiusKey];
    glass.layer.filters = @[frost];
    [panel.contentView addSubview:glass positioned:NSWindowBelow relativeTo:nil];
    neon = [CALayer layer];
    halo = [CALayer layer];
    bloom = makeRim(6, 0.8);
    [halo addSublayer:bloom];
    CIFilter *blur = [CIFilter filterWithName:@"CIGaussianBlur"];
    [blur setValue:@3.5 forKey:kCIInputRadiusKey];
    halo.filters = @[blur];
    [neon addSublayer:halo];
    rim = makeRim(4, 0.95);
    [neon addSublayer:rim];
    [panel.contentView.layer addSublayer:neon];
    layoutRim();
    pinContents(gpuiView.layer);
    void (^outside)(NSEvent *) = ^(NSEvent *event) {
        if (panel.isVisible && !NSPointInRect(NSEvent.mouseLocation, NSInsetRect(panel.frame, 10, 10))) {
            if (actionCallback) actionCallback(5);
        }
    };
    outsideMonitor = [NSEvent addGlobalMonitorForEventsMatchingMask:NSEventMaskLeftMouseDown | NSEventMaskRightMouseDown handler:outside];
    localMonitor = [NSEvent addLocalMonitorForEventsMatchingMask:NSEventMaskLeftMouseDown | NSEventMaskRightMouseDown handler:^NSEvent *(NSEvent *event) { outside(event); return event; }];
    // JIS Eisu/Kana keys have no printable character. Route them to AppKit
    // before GPUI's character-based key translation can discard them.
    inputSourceMonitor = [NSEvent addLocalMonitorForEventsMatchingMask:NSEventMaskKeyDown handler:^NSEvent *(NSEvent *event) {
        if (event.window == panel && panel.isKeyWindow &&
            (event.keyCode == 102 || event.keyCode == 104) &&
            [[gpuiView inputContext] handleEvent:event]) return nil;
        return event;
    }];
    panel.appearance = [NSAppearance appearanceNamed:NSAppearanceNameVibrantDark];
    for (NSNumber *button in @[@(NSWindowCloseButton), @(NSWindowMiniaturizeButton), @(NSWindowZoomButton)])
        [panel standardWindowButton:button.integerValue].hidden = YES;
    status = [[NSStatusBar systemStatusBar] statusItemWithLength:NSSquareStatusItemLength];
    status.button.image = [NSImage imageWithSystemSymbolName:@"waveform.circle" accessibilityDescription:@"Index Voice"];
    menuActions = [IndexMenuActions new];
    NSMenu *menu = [NSMenu new];
    NSArray *titles = @[@"Index Voice · 準備中", @"貼り付けのアクセス許可…", @"リロード", @"終了"];
    for (NSUInteger i = 0; i < titles.count; i++) {
        NSMenuItem *item = [[NSMenuItem alloc] initWithTitle:titles[i] action:i ? @selector(action:) : nil keyEquivalent:@""];
        item.target = menuActions; item.tag = i;
        [menu addItem:item];
    }
    NSMenuItem *settings = [[NSMenuItem alloc] initWithTitle:@"設定…" action:@selector(action:) keyEquivalent:@","];
    settings.target = menuActions; settings.tag = 6;
    [menu insertItem:settings atIndex:1];
    NSMenuItem *history = [[NSMenuItem alloc] initWithTitle:@"文字起こし履歴" action:@selector(action:) keyEquivalent:@""];
    history.target = menuActions; history.tag = 8;
    [menu insertItem:history atIndex:2];
    status.menu = menu;
    [[[NSWorkspace sharedWorkspace] notificationCenter] addObserver:menuActions selector:@selector(spaceChanged:) name:NSWorkspaceActiveSpaceDidChangeNotification object:nil];
    });
}
int index_frontmost_pid(void) {
    pid_t pid = [NSWorkspace sharedWorkspace].frontmostApplication.processIdentifier;
    static pid_t lastExternalPid = 0;
    NSString *bundleID = [NSWorkspace sharedWorkspace].frontmostApplication.bundleIdentifier;
    if (pid > 0 && pid != getpid() && ![bundleID isEqualToString:@"com.apple.loginwindow"]) lastExternalPid = pid;
    return lastExternalPid;
}
void index_status(const char *text) { status.menu.itemArray.firstObject.title = [NSString stringWithUTF8String:text]; }
void index_panel_resize(double height) {
    dispatch_async(dispatch_get_main_queue(), ^{
        [CATransaction begin];
        [CATransaction setDisableActions:YES];
        pinContents(gpuiView.layer);
        NSRect frame = panel.frame;
        frame.size.height = height;
        [panel setFrame:frame display:YES animate:NO];
        layoutRim();
        [gpuiView displayIfNeeded];
        [CATransaction commit];

    });
}
void index_panel_hide(void) { dispatch_async(dispatch_get_main_queue(), ^{ [panel orderOut:nil]; }); }
void index_permission(void) {
    dispatch_async(dispatch_get_main_queue(), ^{
        if (!AXIsProcessTrusted()) {
            AXIsProcessTrustedWithOptions((__bridge CFDictionaryRef)@{(__bridge NSString *)kAXTrustedCheckOptionPrompt: @YES});
        }
    });
}
void index_panel_show(int target) {
    // AppKit callbacks must run after GPUI has released its mutable App borrow.
    dispatch_async(dispatch_get_main_queue(), ^{
    NSScreen *screen = NSScreen.mainScreen ?: NSScreen.screens.firstObject;
    AXUIElementRef app = AXUIElementCreateApplication(target);
    CFTypeRef window = NULL, raw = NULL;
    if (AXIsProcessTrusted() && AXUIElementCopyAttributeValue(app, kAXFocusedWindowAttribute, &window) == kAXErrorSuccess) {
        if (AXUIElementCopyAttributeValue((AXUIElementRef)window, kAXPositionAttribute, &raw) == kAXErrorSuccess && CFGetTypeID(raw) == AXValueGetTypeID()) {
            CGPoint point = CGPointZero;
            AXValueGetValue((AXValueRef)raw, kAXValueCGPointType, &point);
            CFTypeRef sizeValue = NULL;
            if (AXUIElementCopyAttributeValue((AXUIElementRef)window, kAXSizeAttribute, &sizeValue) == kAXErrorSuccess) {
                CGSize size = CGSizeZero;
                if (CFGetTypeID(sizeValue) == AXValueGetTypeID()) AXValueGetValue((AXValueRef)sizeValue, kAXValueCGSizeType, &size);
                point.x += size.width / 2; point.y += size.height / 2;
                CFRelease(sizeValue);
            }
            point.y = NSMaxY(NSScreen.screens.firstObject.frame) - point.y;
            for (NSScreen *candidate in NSScreen.screens) if (NSPointInRect(point, candidate.frame)) { screen = candidate; break; }
        }
    }
    if (raw) CFRelease(raw);
    if (window) CFRelease(window);
    CFRelease(app);
    NSRect frame = screen.visibleFrame;
    NSPoint origin = NSMakePoint(NSMidX(frame) - panel.frame.size.width / 2, NSMinY(frame) + 28);
    BOOL animate = !NSWorkspace.sharedWorkspace.accessibilityDisplayShouldReduceMotion && !panel.isVisible;
    [panel setFrameOrigin:NSMakePoint(origin.x, origin.y - (animate ? 6 : 0))];
    panel.alphaValue = animate ? 0 : 1;
    [panel makeKeyAndOrderFront:nil];
    [panel makeFirstResponder:gpuiView];
    [panel orderFrontRegardless];
    if (actionCallback) actionCallback(4);
    if (animate) {
        [NSAnimationContext runAnimationGroup:^(NSAnimationContext *context) {
            context.duration = 0.16;
            context.timingFunction = [CAMediaTimingFunction functionWithName:kCAMediaTimingFunctionEaseOut];
            panel.animator.alphaValue = 1;
            [panel.animator setFrameOrigin:origin];
        } completionHandler:nil];
    }
    });
}
