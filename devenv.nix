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
  };

  
  packages = [
    pkgs.git
    pkgs.lazygit
    pkgs.pkg-config
    pkgs.vips
  ];

  # See full reference at https://devenv.sh/reference/options/
}
