default:
  just --list

vk *ARGS:
  nix run --impure github:nix-community/nixGL#nixVulkanIntel -- {{ARGS}}
