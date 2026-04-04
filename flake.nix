{
  description = "Rust flake template using rust-overlay and flake-parts.";

  inputs = {
    flake-parts.url = "github:hercules-ci/flake-parts";
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = inputs @ {
    self,
    flake-parts,
    ...
  }: let
    projectName = "rz";
  in
    flake-parts.lib.mkFlake {inherit inputs;} {
      imports = [];
      flake.overlays.rustOverlay = inputs.rust-overlay.overlays.default;
      systems = [
        "x86_64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
        "aarch64-linux"
      ];

      perSystem = {
        config,
        self',
        inputs',
        pkgs,
        system,
        ...
      }: {
        _module.args.pkgs = import inputs.nixpkgs {
          inherit system;
          overlays = [
            self.overlays.rustOverlay
          ];
        };

        formatter = pkgs.alejandra;

        packages = let
          buildRz = features:
            pkgs.rustPlatform.buildRustPackage {
              inherit features;
              pname = projectName;
              version = "0.4.0";
              src = ./.;
              cargoLock.lockFile = ./Cargo.lock;
            };
          features = ["xz2" "bzip2"];
          combinations = list:
            if list == []
            then [[]]
            else let
              rest = combinations (builtins.tail list);
              x = builtins.head list;
            in
              rest ++ map (combo: [x] ++ combo) rest;
          packageCombinations = builtins.filter (c: c != []) (combinations features);
          mkName = combo: "with-" + builtins.concatStringsSep "-" combo;
          mkAttrs = combo: {
            name = mkName combo;
            value = buildRz combo;
          };
          featurePackages = builtins.listToAttrs (map mkAttrs packageCombinations);
        in
          {
            default = buildRz [];
          }
          // featurePackages;
        devShells.default = pkgs.mkShell {
          packages = with pkgs; [
            rust-bin.stable.latest.default
            rust-analyzer
            cargo-nextest
          ];
        };
      };

      flake = {};
    };
}
