{
  description = "woints — Vimium-style keyboard hints for Hyprland";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAll = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
      runtimeLibs = pkgs: [
        pkgs.wayland
        pkgs.libxkbcommon
      ];
      # `features`: cargo features; `onnxruntime`: library exported through
      # `ORT_DYLIB_PATH` (loaded at runtime, never linked). No model weights
      # are ever installed.
      mkWoints =
        pkgs:
        {
          pname ? "woints",
          features ? [ ],
          onnxruntime ? null,
        }:
        pkgs.rustPlatform.buildRustPackage {
          inherit pname;
          version = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).workspace.package.version;
          src = pkgs.lib.cleanSource ./.;
          cargoLock.lockFile = ./Cargo.lock;
          buildFeatures = features;
          nativeBuildInputs = [ pkgs.pkg-config ] ++ pkgs.lib.optional (onnxruntime != null) pkgs.makeWrapper;
          buildInputs = runtimeLibs pkgs;
          # Live tests need a Hyprland session; unit tests run in checkPhase.
          preCheck = pkgs.lib.optionalString (onnxruntime != null) ''
            export ORT_DYLIB_PATH=${onnxruntime}/lib/libonnxruntime.so
          '';
          postInstall =
            pkgs.lib.optionalString (onnxruntime != null) ''
              for b in $out/bin/*; do
                wrapProgram "$b" --set-default ORT_DYLIB_PATH ${onnxruntime}/lib/libonnxruntime.so
              done
            ''
            + ''
              if find $out \( -name '*.onnx' -o -name '*.pt' -o -name '*.safetensors' \) | grep -q .; then
                echo "model weights must not be packaged" >&2
                exit 1
              fi
            '';
          meta = {
            description = "Keyboard hints for Hyprland";
            license = pkgs.lib.licenses.asl20;
            mainProgram = "woints";
            platforms = pkgs.lib.platforms.linux;
          };
        };
    in
    {
      packages = forAll (pkgs: {
        default = mkWoints pkgs { };
        woints-onnx = mkWoints pkgs {
          pname = "woints-onnx";
          features = [ "woints-vision/onnx" ];
          inherit (pkgs) onnxruntime;
        };
        # Best effort, TODO(verify): unfree CUDA, needs `allowUnfree`.
        woints-onnx-cuda = mkWoints pkgs {
          pname = "woints-onnx-cuda";
          features = [ "woints-vision/cuda" ];
          onnxruntime = pkgs.onnxruntime.override { cudaSupport = true; };
        };
        # Best effort, TODO(verify): whether nixpkgs' rocmSupport build
        # includes the MIGraphX execution provider.
        woints-onnx-migraphx = mkWoints pkgs {
          pname = "woints-onnx-migraphx";
          features = [ "woints-vision/migraphx" ];
          onnxruntime = pkgs.onnxruntime.override { rocmSupport = true; };
        };
        quickshell-module = pkgs.runCommand "woints-quickshell-module" { } ''
          mkdir -p $out/share/quickshell/Woints
          cp -v ${./integrations/quickshell}/*.qml ${./integrations/quickshell}/qmldir ${./integrations/quickshell}/README.md $out/share/quickshell/Woints/
          test -s $out/share/quickshell/Woints/SHints.qml
        '';
      });

      devShells = forAll (pkgs: {
        default = pkgs.mkShell {
          packages = [
            pkgs.cargo
            pkgs.rustc
            pkgs.clippy
            pkgs.rustfmt
            pkgs.rust-analyzer
            pkgs.pkg-config
            pkgs.wayland-protocols
            pkgs.just
          ]
          ++ runtimeLibs pkgs;
          LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath (runtimeLibs pkgs);
        };
      });
    };
}
