{
  description = "sellaite: a desktop and an input seat of its own for an AI computer-use agent";
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    flake-parts.url = "github:hercules-ci/flake-parts";
    treefmt-nix = {
      url = "github:numtide/treefmt-nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    leanfran = {
      url = "github:conao3/lean-leanfran/1bb464c6d419da8037698053f52dd23476a042d9";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };
  outputs =
    inputs:
    inputs.flake-parts.lib.mkFlake { inherit inputs; } {
      systems = [
        "x86_64-linux"
        "aarch64-darwin"
      ];
      imports = [ inputs.treefmt-nix.flakeModule ];
      perSystem =
        { pkgs, system, ... }:
        let
          leanfran = inputs.leanfran.packages.${system}.rust-compiler;
          desktopTools = [
            pkgs.sway-unwrapped
            pkgs.grim
            pkgs.python3
            pkgs.coreutils
            pkgs.wayvnc
            pkgs.xwayland
          ];
          atspiLibrary = "${pkgs.at-spi2-core}/lib/libatspi.so.0";
          novncDir = "${pkgs.novnc}/share/webapps/novnc";
          fontsConf = pkgs.makeFontsConf { fontDirectories = [ pkgs.dejavu_fonts ]; };
        in
        {
          treefmt = {
            projectRootFile = "flake.nix";
            programs.nixfmt.enable = true;
            programs.rustfmt.enable = true;
          };
          devShells.default = pkgs.mkShell {
            packages = [
              pkgs.cargo
              pkgs.cargo-deny
              pkgs.rustc
              pkgs.clippy
              pkgs.rustfmt
              pkgs.rust-analyzer
              pkgs.lean4
              pkgs.gnumake
              leanfran
            ]
            ++ pkgs.lib.optionals pkgs.stdenv.hostPlatform.isLinux (
              desktopTools
              ++ [
                pkgs.foot
                pkgs.xterm
              ]
            );
            LEANFRAN_CLI = "${leanfran}/bin/leanfran";
            FONTCONFIG_FILE = fontsConf;
            SELLAITE_NOVNC_DIR = novncDir;
            SELLAITE_ATSPI_LIBRARY = pkgs.lib.optionalString pkgs.stdenv.hostPlatform.isLinux atspiLibrary;
          };
          packages = pkgs.lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
            default = pkgs.rustPlatform.buildRustPackage {
              pname = "sellaite";
              inherit ((builtins.fromTOML (builtins.readFile ./Cargo.toml)).package) version;
              src = ./.;
              cargoLock.lockFile = ./Cargo.lock;
              nativeBuildInputs = [ pkgs.makeWrapper ];
              LEANFRAN_OFFLINE = "1";
              nativeCheckInputs = desktopTools ++ [
                pkgs.foot
                pkgs.xterm
              ];
              FONTCONFIG_FILE = fontsConf;
              SELLAITE_NOVNC_DIR = novncDir;
              postInstall = "wrapProgram $out/bin/sellaite --prefix PATH : ${pkgs.lib.makeBinPath desktopTools} --set-default SELLAITE_NOVNC_DIR ${novncDir} --set-default SELLAITE_ATSPI_LIBRARY ${atspiLibrary}";
              meta = {
                description = "A desktop and an input seat of its own for an AI computer-use agent";
                mainProgram = "sellaite";
                license = pkgs.lib.licenses.asl20;
                platforms = pkgs.lib.platforms.linux;
              };
            };
          };
        };
    };
}
