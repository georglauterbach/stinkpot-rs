{
  description = "sqlite-backed shell history";

  inputs.nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";

  outputs = {
    self,
    nixpkgs,
  }: let
    supportedSystems = ["x86_64-linux" "aarch64-linux" "x86_64-darwin" "aarch64-darwin"];
    forAllSystems = f: nixpkgs.lib.genAttrs supportedSystems (system: f nixpkgs.legacyPackages.${system});

    stinkpotPackage = {
      lib,
      buildGoModule,
    }:
      buildGoModule {
        pname = "stinkpot";
        version = "0.1.0";
        src = lib.cleanSource ./.;
        vendorHash = "sha256-iDlU/176inkilehXft25KjiLt7rUtlMGqod22A3O/ko=";
        env.CGO_ENABLED = "0";
        ldflags = ["-s" "-w"];
        meta = {
          description = "sqlite-backed shell history";
          mainProgram = "stinkpot";
          platforms = lib.platforms.unix;
        };
      };
  in {
    formatter = forAllSystems (pkgs: pkgs.alejandra);

    packages = forAllSystems (pkgs: rec {
      stinkpot = pkgs.callPackage stinkpotPackage {};
      default = stinkpot;
    });

    overlays.default = final: _prev: {
      stinkpot = final.callPackage stinkpotPackage {};
    };

    homeManagerModules.stinkpot = {
      config,
      lib,
      pkgs,
      ...
    }: let
      cfg = config.programs.stinkpot;
    in {
      options.programs.stinkpot = {
        enable = lib.mkEnableOption "stinkpot, a tiny sqlite-backed shell history";

        package = lib.mkOption {
          type = lib.types.package;
          default = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
          description = "The stinkpot package to use.";
        };

        enableBashIntegration = lib.hm.shell.mkBashIntegrationOption {
          inherit config;
          extraDescription = "Bind `ctrl-r` to open the stinkpot search and record commands into stinkpot.";
        };
      };

      config = lib.mkIf cfg.enable {
        home.packages = [cfg.package];

        programs.bash.initExtra = lib.mkIf cfg.enableBashIntegration ''
          eval "$(${lib.getExe cfg.package} init)"
        '';
      };
    };
    homeManagerModules.default = self.homeManagerModules.stinkpot;

    devShells = forAllSystems (pkgs: {
      default = pkgs.mkShell {
        packages = [
          pkgs.go
          pkgs.gopls
          pkgs.gotools
          pkgs.go-tools
          pkgs.litecli
        ];
      };
    });
  };
}
