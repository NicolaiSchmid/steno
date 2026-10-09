# The Linux desktop app, built from this repository's source the way
# desktop-release.yml builds the `.deb`: the web UI, the speech sidecar
# (apps/desktop/scripts/stage-sidecar.sh), then `cargo tauri build` with
# tauri.release.conf.json, whose `.deb` tree becomes $out. Every hash is in
# the tree (the crates' in Cargo.lock, the web UI's below), so any tag
# builds as it is. Item X7 of .plans/2026-10-07-stable-promotion.md.
{
  lib,
  rustPlatform,
  fetchFromGitHub,
  cargo-tauri,
  fetchPnpmDeps,
  pnpmConfigHook,
  pnpm_11,
  nodejs,
  pkg-config,
  wrapGAppsHook3,
  webkitgtk_4_1,
  gtk3,
  glib,
  glib-networking,
  libsoup_3,
  libayatana-appindicator,
  pipewire,
  dbus,
  onnxruntime,
}: let
  root = ../.;
  fs = lib.fileset;
  # Everything but what no Rust or web build reads (the Swift app, the
  # phone, the site, plans, docs and the Nix files themselves), so editing
  # those leaves the package as it was.
  src = fs.toSource {
    inherit root;
    fileset = fs.difference root (fs.unions [
      ../.agents
      ../.claude
      ../.github
      ../.plans
      ../apps/macos/Steno
      ../apps/macos/StenoTests
      ../apps/macos/StenoUITests
      ../apps/site
      ../docs
      ../mobile
      ../nix
      ../spikes
      ../Tests
      ../flake.lock
      ../flake.nix
    ]);
  };
  web = ../apps/macos/web;
  # The CLI the release builds with (desktop-release.yml pins 2.12.1);
  # nixpkgs' 2.11 refuses tauri.conf.json's `bundleVCRuntime`.
  tauri = cargo-tauri.overrideAttrs (final: _: {
    version = "2.12.1";
    src = fetchFromGitHub {
      owner = "tauri-apps";
      repo = "tauri";
      tag = "tauri-cli-v${final.version}";
      hash = "sha256-k62c9YuU76Xx++HL8z0lEeE2LVQdYAcEE1cJR+GTppo=";
    };
    cargoDeps = rustPlatform.fetchCargoVendor {
      inherit (final) pname version src;
      hash = "sha256-cgaGCjOE27tXUSgg/VAdlfllIef2tNUlT7Gg6gHOQkc=";
    };
    # nixpkgs tests the CLI at its own version.
    doCheck = false;
  });
  workspace = (lib.importTOML ../Cargo.toml).workspace.package;
in
  rustPlatform.buildRustPackage (finalAttrs: {
    pname = "steno-desktop";
    inherit (workspace) version;
    inherit src;

    # Keeps the JSON in `tauriBuildFlags` one argument.
    __structuredAttrs = true;

    cargoLock.lockFile = ../Cargo.lock;

    # The web UI's own pnpm project. Bump `hash` with its lockfile: build,
    # and copy the hash the mismatch prints.
    pnpmRoot = "apps/macos/web";
    pnpmDeps = fetchPnpmDeps {
      inherit (finalAttrs) pname version;
      src = fs.toSource {
        root = web;
        fileset = fs.unions [
          (web + "/package.json")
          (web + "/pnpm-lock.yaml")
          (web + "/pnpm-workspace.yaml")
        ];
      };
      pnpm = pnpm_11;
      fetcherVersion = 4;
      hash = "sha256-vWZhVb98QJPo1DQO5ci7khV/cLC0MBfp3SNTWa1D4A8=";
    };

    nativeBuildInputs = [
      tauri.hook
      nodejs
      pnpm_11
      pnpmConfigHook
      pkg-config
      rustPlatform.bindgenHook
      wrapGAppsHook3
    ];

    buildInputs = [
      dbus
      glib
      glib-networking
      gtk3
      libayatana-appindicator
      libsoup_3
      onnxruntime
      pipewire
      webkitgtk_4_1
    ];

    env = {
      # nixpkgs' ONNX Runtime, linked dynamically, in place of the build
      # `ort` downloads, which the sandbox forbids. Its telemetry stays off
      # as everywhere: `init_environment` in crates/steno-speech/src/onnx.rs.
      ORT_LIB_LOCATION = "${lib.getLib onnxruntime}/lib";
      ORT_PREFER_DYNAMIC_LINK = "1";
      # The build-time default of the distribution switch (X5): updates come
      # from the package manager. The wrapper sets it too.
      STENO_DISTRIBUTION = "nix";
    };

    # The tray library `dlopen`s libayatana-appindicator by its soname,
    # which nothing on NixOS resolves; name the store path instead.
    postPatch = ''
      indicator="$(find "$cargoDepsCopy" -path '*/libappindicator-sys-*/src/lib.rs')"
      substituteInPlace "$indicator" --replace-fail \
        '"libayatana-appindicator3.so.1"' '"${finalAttrs.passthru.trayLibrary}"'
    '';

    # The sidecar, staged as tauri.release.conf.json's `externalBin`; the
    # bundler installs it beside the app's binary, where
    # `SidecarConfig::beside_current_exe` looks.
    preBuild = ''
      bash apps/desktop/scripts/stage-sidecar.sh
    '';

    buildAndTestSubdir = "apps/desktop/src-tauri";
    tauriBuildFlags = [
      "--config"
      "tauri.release.conf.json"
      # No updater artifacts: they need the release signing key, and the
      # store is read-only anyway.
      "--config"
      (builtins.toJSON {bundle.createUpdaterArtifacts = false;})
    ];

    # Rust CI runs the tests; the build here is the package only.
    doCheck = false;

    # Only the app gets the GTK wrapper. The sidecar stays the real binary
    # beside the wrapped one: the wrapper execs
    # $out/bin/.steno-desktop-wrapped, whose `current_exe` is in $out/bin.
    dontWrapGApps = true;
    preFixup = ''
      gappsWrapperArgs+=(--set-default STENO_DISTRIBUTION nix)
    '';
    postFixup = ''
      wrapGApp "$out/bin/steno-desktop"
    '';

    passthru = {
      # The library the tray opens, by store path (`postPatch`).
      trayLibrary = "${lib.getLib libayatana-appindicator}/lib/libayatana-appindicator3.so.1";
    };

    meta = {
      description = "Bot-free meeting recorder: records calls locally, transcribes and summarises on-device";
      homepage = "https://github.com/NicolaiSchmid/steno";
      license = lib.licenses.mit;
      mainProgram = "steno-desktop";
      platforms = ["x86_64-linux"];
      sourceProvenance = [lib.sourceTypes.fromSource];
    };
  })
