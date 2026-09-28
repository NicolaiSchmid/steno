{
  description = "Steno: a bot-free meeting recorder for the Mac (installs the released Steno.app)";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";

  outputs = {
    self,
    nixpkgs,
  }: let
    # Bump both lines on every release. `release.yml` prints them in the
    # job summary of the tag run, so a bump is copy-paste from there.
    release = {
      version = "0.9.0-rc.1";
      hash = "sha256-y3DjnJIpvkSDSi9vM8YL6JfJW1At70/WkjWxIxR7fd8=";
    };

    # The app is a signed, notarised Apple Silicon bundle; there is nothing
    # to build on any other system.
    system = "aarch64-darwin";
    pkgs = nixpkgs.legacyPackages.${system};
    lib = pkgs.lib;

    bundleId = "uno.schmid.steno.mac";

    steno = pkgs.stdenvNoCC.mkDerivation {
      pname = "steno";
      inherit (release) version;

      src = pkgs.fetchurl {
        url = "https://github.com/NicolaiSchmid/steno/releases/download/v${release.version}/Steno-${release.version}.dmg";
        inherit (release) hash;
      };

      nativeBuildInputs = [pkgs.undmg];
      sourceRoot = ".";

      # Prebuilt, Developer ID signed and notarised. Every phase that would
      # touch the bundle is off: stripping, shebang patching or re-signing
      # would break the signature that Gatekeeper checks on first launch.
      dontConfigure = true;
      dontBuild = true;
      dontPatchShebangs = true;
      dontFixup = true;

      installPhase = ''
        runHook preInstall
        mkdir -p $out/Applications
        cp -R Steno.app $out/Applications/
        runHook postInstall
      '';

      # Sparkle stays in the bundle (removing it would invalidate the
      # signature) but cannot replace an app inside the read-only Nix store.
      # The note tells a Nix user how to turn scheduled checks off; bumping
      # `release` above and rebuilding is the update path.
      postInstall = ''
        mkdir -p $out/share/doc/steno
        cat > $out/share/doc/steno/UPDATES.md <<EOF
        # Updating a Nix-installed Steno

        This Steno.app runs from the read-only Nix store, so Sparkle's
        "Check for Updates…" can download a new version but cannot install
        it. Update by bumping \`release.version\` and \`release.hash\` in
        flake.nix (or by updating the flake input that pins this repository)
        and rebuilding.

        To stop the scheduled daily check from offering updates it cannot
        apply:

            defaults write ${bundleId} SUEnableAutomaticChecks -bool NO

        Homebrew users (\`brew install nicolaischmid/tap/steno\`) keep the
        in-app updater.
        EOF
      '';

      meta = {
        description = "Bot-free meeting recorder for the Mac: records calls locally, transcribes and summarises on-device";
        homepage = "https://github.com/NicolaiSchmid/steno";
        changelog = "https://github.com/NicolaiSchmid/steno/releases/tag/v${release.version}";
        # TODO: README.md says MIT but the repository has no LICENSE file
        # yet. Add one and this line is right; until then nixpkgs' license
        # checks would call the package unlicensed.
        license = lib.licenses.mit;
        platforms = [system];
        sourceProvenance = [lib.sourceTypes.binaryNativeCode];
      };
    };
  in {
    packages.${system} = {
      inherit steno;
      default = steno;
    };

    checks.${system} = {
      # The unpacked bundle is a real app with the expected identity. The
      # signature itself is verified outside the sandbox (codesign, spctl
      # in the release plan); this only catches an unpack or copy that
      # produced a different bundle.
      steno-bundle =
        pkgs.runCommand "steno-bundle-check" {
          nativeBuildInputs = [pkgs.python3];
          app = "${steno}/Applications/Steno.app";
          expected = bundleId;
        } ''
          test -f "$app/Contents/Info.plist"
          test -x "$app/Contents/MacOS/Steno"
          test -d "$app/Contents/_CodeSignature"
          python3 - "$app/Contents/Info.plist" "$expected" <<'PY'
          import plistlib, sys
          info = plistlib.load(open(sys.argv[1], "rb"))
          got = info.get("CFBundleIdentifier")
          assert got == sys.argv[2], f"CFBundleIdentifier is {got!r}, expected {sys.argv[2]!r}"
          assert info.get("CFBundleShortVersionString") == "${release.version}", info.get("CFBundleShortVersionString")
          print("ok", got, info.get("CFBundleShortVersionString"), "build", info.get("CFBundleVersion"))
          PY
          touch $out
        '';
    };

    formatter = lib.genAttrs [system "x86_64-linux" "aarch64-linux"] (
      fmtSystem: nixpkgs.legacyPackages.${fmtSystem}.alejandra
    );
  };
}
