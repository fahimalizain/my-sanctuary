{
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }: let
    systems = [ "aarch64-darwin" "x86_64-linux" ];
    forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
  in {
    devShells = forAllSystems (pkgs: {
      default = pkgs.mkShell {
        packages = with pkgs; [
          rustup        # cargo/rustc shims; rust-toolchain.toml pins stable + wasm32-unknown-unknown
          rust-analyzer # LSP
        ];
      };
    });
  };
}
