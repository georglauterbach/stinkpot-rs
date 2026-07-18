{
  description = "sqlite-backed shell history";

  inputs.nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";

  outputs = {
    self,
    nixpkgs,
  }: let
    supportedSystems = ["x86_64-linux" "aarch64-linux" "x86_64-darwin" "aarch64-darwin"];
    forAllSystems = f: nixpkgs.lib.genAttrs supportedSystems (system: f nixpkgs.legacyPackages.${system});

    tortuPackage = {
      lib,
      buildGoModule,
    }:
      buildGoModule {
        pname = "tortu";
        version = "0.1.0";
        src = lib.cleanSource ./.;
        vendorHash = "sha256-iDlU/176inkilehXft25KjiLt7rUtlMGqod22A3O/ko=";
        env.CGO_ENABLED = "0";
        ldflags = ["-s" "-w"];
        meta = {
          description = "sqlite-backed shell history";
          mainProgram = "tortu";
          platforms = lib.platforms.unix;
        };
      };
  in {
    formatter = forAllSystems (pkgs: pkgs.alejandra);

    packages = forAllSystems (pkgs: rec {
      tortu = pkgs.callPackage tortuPackage {};
      default = tortu;
    });

    overlays.default = final: _prev: {
      tortu = final.callPackage tortuPackage {};
    };

    homeManagerModules.tortu = {
      config,
      lib,
      pkgs,
      ...
    }: let
      cfg = config.programs.tortu;
    in {
      options.programs.tortu = {
        enable = lib.mkEnableOption "tortu, a tiny sqlite-backed shell history";

        package = lib.mkOption {
          type = lib.types.package;
          default = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
          description = "The tortu package to use.";
        };

        enableBashIntegration = lib.hm.shell.mkBashIntegrationOption {
          inherit config;
          extraDescription = "Bind `ctrl-r` to open the tortu search and record commands into tortu.";
        };
      };

      config = lib.mkIf cfg.enable {
        home.packages = [cfg.package];

        programs.bash.initExtra = lib.mkIf cfg.enableBashIntegration ''
          eval "$(${lib.getExe cfg.package} init)"
        '';
      };
    };
    homeManagerModules.default = self.homeManagerModules.tortu;

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
