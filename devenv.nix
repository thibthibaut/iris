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
  ];

  # See full reference at https://devenv.sh/reference/options/
}
