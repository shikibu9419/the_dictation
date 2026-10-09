#import <AppKit/AppKit.h>
#import <ApplicationServices/ApplicationServices.h>
#import <QuartzCore/QuartzCore.h>
#import <CoreImage/CoreImage.h>
#import <Carbon/Carbon.h>

static NSWindow *panel;
static NSView *gpuiView;
static BOOL editingText = NO;
static BOOL circularPanel = NO;
static pid_t returnTarget = 0;
static BOOL sourceIsJapanese(TISInputSourceRef source) {
    NSArray *languages = (__bridge NSArray *)TISGetInputSourceProperty(source, kTISPropertyInputSourceLanguages);
    return [languages containsObject:@"ja"];
}
static BOOL selectKeyboardSource(BOOL japanese) {
    NSTextInputContext *context = [gpuiView inputContext];
    if (!context) { NSLog(@"[Index IME] Missing text input context"); return NO; }
    NSDictionary *filter = @{
        (__bridge NSString *)kTISPropertyInputSourceIsEnabled: @YES,
        (__bridge NSString *)kTISPropertyInputSourceIsSelectCapable: @YES,
        (__bridge NSString *)kTISPropertyInputSourceCategory: (__bridge NSString *)kTISCategoryKeyboardInputSource
    };
    NSArray *sources = CFBridgingRelease(TISCreateInputSourceList((__bridge CFDictionaryRef)filter, false));
    for (id value in sources) {
        TISInputSourceRef source = (__bridge TISInputSourceRef)value;
        BOOL ascii = [(__bridge NSNumber *)TISGetInputSourceProperty(source, kTISPropertyInputSourceIsASCIICapable) boolValue];
        if (japanese ? !sourceIsJapanese(source) : (!ascii || sourceIsJapanese(source))) continue;
        NSString *identifier = (__bridge NSString *)TISGetInputSourceProperty(source, kTISPropertyInputSourceID);
        context.allowedInputSourceLocales = nil;
        [context activate];
        OSStatus result = TISSelectInputSource(source);
        if (result == noErr) context.selectedKeyboardInputSource = identifier;
        NSLog(@"[Index IME] select=%@ status=%d context_source=%@ active=%d key=%d", identifier, (int)result, context.selectedKeyboardInputSource, NSApp.isActive, panel.isKeyWindow);
        if (result == noErr) return YES;
    }
    NSLog(@"[Index IME] No selectable source for japanese=%d", japanese);
    return NO;
}
static NSVisualEffectView *glass;
static CALayer *neon, *halo;
static CAGradientLayer *rim, *bloom;
static NSTimer *audioTimer;
static double audioTarget, audioEnvelope, audioUpdated;
static void applyAudioGlow(double level) {
    [CATransaction begin];
    [CATransaction setDisableActions:YES];
    CIFilter *blur = [CIFilter filterWithName:@"CIGaussianBlur"];
    [blur setValue:@(3.5 + 5.5 * level) forKey:kCIInputRadiusKey];
    halo.filters = @[blur];
    ((CAShapeLayer *)bloom.mask).lineWidth = 6 + 7 * level;
    bloom.opacity = 0.8 + 0.2 * level;
    [CATransaction commit];
}
void index_panel_audio(double level, bool active) {
    dispatch_async(dispatch_get_main_queue(), ^{
        if (!active) {
            [audioTimer invalidate]; audioTimer = nil;
            audioTarget = audioEnvelope = 0;
            applyAudioGlow(0);
            return;
        }
        audioTarget = isfinite(level) ? fmax(0, fmin(1, level)) : 0;
        audioUpdated = CACurrentMediaTime();
        // React in this main-queue turn. Waiting for the next timer tick and
        // averaging the attack again added visible lag to every BLE update.
        audioEnvelope = audioTarget;
        applyAudioGlow(audioEnvelope);
        if (audioTimer) return;
        audioTimer = [NSTimer timerWithTimeInterval:1.0 / 60 repeats:YES block:^(NSTimer *timer) {
            double age = CACurrentMediaTime() - audioUpdated;
            // Decay only when no fresh level is available, with no second
            // low-pass filter that keeps old speech glowing after it ended.
            audioEnvelope = audioTarget * exp(-fmax(0, age - 0.10) / 0.12);
            applyAudioGlow(audioEnvelope);
        }];
        [[NSRunLoop mainRunLoop] addTimer:audioTimer forMode:NSRunLoopCommonModes];
    });
}

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
    CGFloat inset = circularPanel ? 20 : 10;
    glass.frame = NSInsetRect(bounds, inset, inset);
    CGFloat radius = circularPanel ? (MIN(bounds.size.width, bounds.size.height) - 2 * inset) / 2 : 18;
    glass.layer.cornerRadius = radius;
    CGPathRef path = CGPathCreateWithRoundedRect(CGRectInset(bounds, inset, inset), radius, radius, NULL);
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
    // Select an enabled keyboard source explicitly. handleEvent: alone does
    // not guarantee that a language-switch key changes the selected source.
    inputSourceMonitor = [NSEvent addLocalMonitorForEventsMatchingMask:NSEventMaskKeyDown handler:^NSEvent *(NSEvent *event) {
        if (event.window != panel || !panel.isKeyWindow || !editingText) return event;
        if ((event.keyCode == 102 || event.keyCode == 104) && selectKeyboardSource(event.keyCode == 104)) return nil;
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
void index_panel_resize(double width, double height, bool circular) {
    dispatch_async(dispatch_get_main_queue(), ^{
        [CATransaction begin];
        [CATransaction setDisableActions:YES];
        pinContents(gpuiView.layer);
        NSRect frame = panel.frame;
        frame.origin.x += (frame.size.width - width) / 2;
        frame.size.width = width;
        frame.size.height = height;
        circularPanel = circular;
        [panel setFrame:frame display:YES animate:NO];
        layoutRim();
        [gpuiView displayIfNeeded];
        [CATransaction commit];

    });
}
// Hidden windows have no display-link ticks. Explicitly render the prepared
// surface before the queued show operation; run outside GPUI's App borrow.
void index_panel_request_frame(void) {
    dispatch_async(dispatch_get_main_queue(), ^{
        [gpuiView.layer setNeedsDisplay];
        [gpuiView.layer displayIfNeeded];
    });
}
static void activateTextInput(void) {
    if (!editingText || !panel.isVisible) return;
    [NSApp activateIgnoringOtherApps:YES];
    [panel makeKeyAndOrderFront:nil];
    [panel makeFirstResponder:gpuiView];
    [[gpuiView inputContext] activate];
    NSLog(@"[Index IME] editing active=%d key=%d responder=%@ context=%@ source=%@", NSApp.isActive, panel.isKeyWindow, NSStringFromClass(panel.firstResponder.class), [gpuiView inputContext], [gpuiView inputContext].selectedKeyboardInputSource);
    if (actionCallback) actionCallback(4);
}
void index_panel_editing(bool editing) {
    editingText = editing;
    dispatch_async(dispatch_get_main_queue(), ^{
        if (editingText) activateTextInput();
        else [[gpuiView inputContext] deactivate];
    });
}
void index_panel_hide(void) { index_panel_audio(0, false); dispatch_async(dispatch_get_main_queue(), ^{
    [[gpuiView inputContext] deactivate];
    [panel orderOut:nil];
    NSLog(@"[Index panel] hidden visible=%d", panel.isVisible);
    if (NSApp.isActive && returnTarget > 0) {
        [[NSRunningApplication runningApplicationWithProcessIdentifier:returnTarget] activateWithOptions:0];
    }
}); }
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
    returnTarget = target;
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
    // Preserve the panel's keyboard handling; only explicitly activate the
    // application when editing so AppKit can manage the IME input context.
    [panel makeKeyAndOrderFront:nil];
    [panel makeFirstResponder:gpuiView];
    [panel orderFrontRegardless];
    if (editingText) activateTextInput();
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
