{
  pkgs,
  lib,
  config,
  ...
}:
{
  # https://devenv.sh/languages/
  languages.rust = {
    enable = true;
    rustflags = "-C target-cpu=native";
  };
  
  packages = [
    pkgs.git
    pkgs.lazygit
    pkgs.pkg-config
    pkgs.vips
    pkgs.openssl
    pkgs.llvmPackages.libclang
    pkgs.llvmPackages.libcxxClang
  ];

    env = {
    # Point bindgen to the correct libclang directory inside the Nix store
    LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
    };
  # See full reference at https://devenv.sh/reference/options/
}
