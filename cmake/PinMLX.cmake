# First FetchContent declaration wins, including the later declaration in mlx-c.
# Pin the MLX commit behind v0.30.6 as well as the mlx-c submodule revision.
include(FetchContent)
FetchContent_Declare(
  mlx
  GIT_REPOSITORY https://github.com/ml-explore/mlx.git
  GIT_TAG 185b06d9efc1c869540eccfb5baff853fff3659d)
