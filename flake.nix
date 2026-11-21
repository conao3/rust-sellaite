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
          ];
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
                pkgs.wayvnc
                pkgs.foot
              ]
            );
            LEANFRAN_CLI = "${leanfran}/bin/leanfran";
            FONTCONFIG_FILE = fontsConf;
          };
          packages = pkgs.lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
            default = pkgs.rustPlatform.buildRustPackage {
              pname = "sellaite";
              version = "0.1.0";
              src = ./.;
              cargoLock.lockFile = ./Cargo.lock;
              nativeBuildInputs = [ pkgs.makeWrapper ];
              LEANFRAN_OFFLINE = "1";
              nativeCheckInputs = desktopTools ++ [ pkgs.foot ];
              FONTCONFIG_FILE = fontsConf;
              postInstall = "wrapProgram $out/bin/sellaite --prefix PATH : ${pkgs.lib.makeBinPath desktopTools}";
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
