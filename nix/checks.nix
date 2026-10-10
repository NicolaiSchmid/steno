# The Linux checks behind `nix flake check`: the package's layout, libraries
# and wrapper, and the NixOS module evaluated in minimal systems (a
# system-wide and a per-user install down to the user units it generates,
# the session variables and the firewall, one without launch at login or
# the firewall, and one each with OpenSSH's and GnuPG's SSH agent).
{
  nixpkgs,
  self,
  pkgs,
  steno,
}: let
  lib = pkgs.lib;
  conf = lib.importJSON ../apps/desktop/src-tauri/tauri.conf.json;
  # The `.deb`'s systemd user files (P5's stop timeout drop-ins), as
  # `<path below lib/systemd/user/> <file in src-tauri/>` lines: the package
  # holds each one where the `.deb` puts it.
  userUnitFiles = lib.concatStrings (lib.mapAttrsToList (
    target: source: let
      below = lib.removePrefix "/usr/lib/systemd/user/" target;
    in
      lib.optionalString (below != target) "${below}\t${source}\n"
  ) (conf.bundle.linux.deb.files or {}));

  system = modules:
    nixpkgs.lib.nixosSystem {
      modules =
        [
          self.nixosModules.default
          {
            nixpkgs.hostPlatform = "x86_64-linux";
            boot.loader.grub.enable = false;
            fileSystems."/" = {
              device = "none";
              fsType = "tmpfs";
            };
            system.stateVersion = "26.11";
            programs.steno.enable = true;
          }
        ]
        ++ modules;
    };
  systemWide = system [];
  perUser = system [
    {
      users.users.alice.isNormalUser = true;
      programs.steno.users = ["alice"];
      programs.steno.inhibitDelayMaxSec = 15;
      programs.steno.handoverPort = 23900;
    }
  ];
  noLogin = system [
    {
      programs.steno.launchAtLogin = false;
      programs.steno.openFirewall = false;
    }
  ];
  # Another SSH agent: the keyring default must stay off and evaluate.
  withAgent = system [{programs.ssh.startAgent = true;}];
  withGpgAgent = system [
    {
      programs.gnupg.agent = {
        enable = true;
        enableSSHSupport = true;
      };
    }
  ];
  userUnits = config: config.environment.etc."systemd/user".source;
  installs = config: lib.elem steno config.environment.systemPackages;
  # What `nixos-rebuild` would refuse with.
  evaluates = config: lib.all (a: a.assertion) config.assertions;

  # The port the app binds on Linux, which the module's default must be.
  linuxPorts = map lib.head (lib.filter lib.isList (builtins.split
    "pub const LINUX_PORT: u16 = ([0-9]+);"
    (builtins.readFile ../crates/steno-handover/src/configuration.rs)));
  defaultPort = systemWide.config.programs.steno.handoverPort;
  firewall = config: config.networking.firewall;
  opens = port: config:
    lib.elem port (firewall config).allowedTCPPorts
    && lib.elem 5353 (firewall config).allowedUDPPorts;
  portVariable = config: config.environment.sessionVariables.STENO_HANDOVER_PORT or null;

  # Every Tauri CLI the release installs is the one the package builds with.
  tauriVersion = steno.passthru.tauriCli.version;
  releaseTauriVersions = map lib.head (lib.filter lib.isList (builtins.split
    "@tauri-apps/cli@([^ ]+) "
    (builtins.readFile ../.github/workflows/desktop-release.yml)));
in {
  package = assert lib.assertMsg (releaseTauriVersions != [] && lib.all (v: v == tauriVersion) releaseTauriVersions)
  "nix/package.nix builds with Tauri CLI ${tauriVersion}, desktop-release.yml with ${toString releaseTauriVersions}";
  assert lib.assertMsg (steno.env.STENO_DISTRIBUTION == "nix")
  "the package is built with STENO_DISTRIBUTION=${steno.env.STENO_DISTRIBUTION}, not nix";
    pkgs.runCommand "steno-package-check" {
      nativeBuildInputs = [pkgs.file];
      inherit userUnitFiles;
      src = ../apps/desktop/src-tauri;
      tray = steno.passthru.trayLibrary;
      onnxruntime = lib.getLib pkgs.onnxruntime;
    } ''
      set -euo pipefail
      bin=${steno}/bin
      ldd=${lib.getBin pkgs.glibc}/bin/ldd

      # The wrapper execs the real binary in the same directory, so the
      # sidecar beside it is the one `SidecarConfig::beside_current_exe`
      # finds; the sidecar itself is not wrapped.
      grep -q "$bin/.steno-desktop-wrapped" "$bin/steno-desktop" \
        || { echo "the wrapper does not exec .steno-desktop-wrapped"; exit 1; }
      # makeBinaryWrapper embeds its C source: this is the --set-default.
      grep -qaF 'setenv("STENO_DISTRIBUTION", "nix", 0)' "$bin/steno-desktop" \
        || { echo "the wrapper does not default STENO_DISTRIBUTION to nix"; exit 1; }
      # GTK's schemas, without which the file chooser aborts the app, and
      # GIO's TLS module.
      grep -qF 'gsettings-schemas/${pkgs.gtk3.name}' "$bin/steno-desktop" \
        || { echo "the wrapper misses GTK's schemas"; exit 1; }
      grep -qF '${pkgs.glib-networking}/lib/gio/modules' "$bin/steno-desktop" \
        || { echo "the wrapper misses glib-networking"; exit 1; }
      for elf in .steno-desktop-wrapped steno-speech-sidecar; do
        file -L "$bin/$elf" | grep -q ELF || { echo "$elf is not an ELF binary"; exit 1; }
        libs="$($ldd "$bin/$elf")"
        if grep 'not found' <<< "$libs"; then echo "$elf misses a library"; exit 1; fi
        # nixpkgs' ONNX Runtime, linked.
        grep -q "$onnxruntime/lib/libonnxruntime.so" <<< "$libs" \
          || { echo "$elf does not link nixpkgs' ONNX Runtime"; exit 1; }
      done

      # The tray opens libayatana-appindicator by this store path.
      test -f "$tray" || { echo "the tray's library is missing"; exit 1; }
      grep -qaF "$tray" "$bin/.steno-desktop-wrapped" \
        || { echo "the binary does not name the tray's library"; exit 1; }

      grep -q '^Exec=steno-desktop %u$' ${steno}/share/applications/steno-desktop.desktop \
        || { echo "the desktop entry's Exec is not steno-desktop %u"; exit 1; }

      while IFS=$'\t' read -r below source; do
        [ -n "$below" ] || continue
        cmp "${steno}/lib/systemd/user/$below" "$src/$source" \
          || { echo "lib/systemd/user/$below differs from $source"; exit 1; }
        cmp "${steno}/share/systemd/user/$below" "$src/$source" \
          || { echo "share/systemd/user/$below differs from $source"; exit 1; }
        echo "ok: lib/systemd/user/$below, also under share/"
      done <<< "$userUnitFiles"
      touch $out
    '';

  module = assert installs systemWide.config;
  assert !(installs perUser.config);
  assert lib.elem steno perUser.config.users.users.alice.packages;
  assert evaluates systemWide.config && evaluates perUser.config && evaluates noLogin.config;
  assert evaluates withAgent.config && evaluates withGpgAgent.config;
  assert systemWide.config.services.pipewire.enable;
  assert systemWide.config.services.gnome.gnome-keyring.enable;
  assert !withAgent.config.services.gnome.gnome-keyring.enable;
  assert !withGpgAgent.config.services.gnome.gnome-keyring.enable;
  assert !(systemWide.config.services.logind.settings.Login ? InhibitDelayMaxSec);
  assert perUser.config.services.logind.settings.Login.InhibitDelayMaxSec == 15;
  assert lib.assertMsg (systemWide.config.environment.sessionVariables.STENO_LOGIN_ITEM or null == "managed")
  "the session lacks STENO_LOGIN_ITEM=managed";
  assert lib.assertMsg (perUser.config.environment.sessionVariables.STENO_LOGIN_ITEM or null == "managed")
  "the per-user session lacks STENO_LOGIN_ITEM=managed";
  assert lib.assertMsg (!(noLogin.config.environment.sessionVariables ? STENO_LOGIN_ITEM))
  "launchAtLogin = false still sets STENO_LOGIN_ITEM";
  assert lib.assertMsg (!(noLogin.config.systemd.user.services ? steno))
  "launchAtLogin = false still has steno.service";
  assert lib.assertMsg (linuxPorts == [(toString defaultPort)])
  "handoverPort defaults to ${toString defaultPort}, crates/steno-handover's LINUX_PORT is ${toString linuxPorts}";
  assert lib.assertMsg (opens defaultPort systemWide.config && portVariable systemWide.config == toString defaultPort)
  "the firewall does not open ${toString defaultPort}/tcp and 5353/udp, or the session's STENO_HANDOVER_PORT is not ${toString defaultPort}";
  assert lib.assertMsg (opens 23900 perUser.config && !(lib.elem defaultPort (firewall perUser.config).allowedTCPPorts) && portVariable perUser.config == "23900")
  "handoverPort = 23900 does not move the firewall's port and STENO_HANDOVER_PORT";
  assert lib.assertMsg (!(lib.elem defaultPort (firewall noLogin.config).allowedTCPPorts) && !(lib.elem 5353 (firewall noLogin.config).allowedUDPPorts) && portVariable noLogin.config == toString defaultPort)
  "openFirewall = false still opens a port, or drops STENO_HANDOVER_PORT";
    pkgs.runCommand "steno-module-check" {
      inherit userUnitFiles;
      systemWideUnits = userUnits systemWide.config;
      perUserUnits = userUnits perUser.config;
    } ''
      set -euo pipefail
      # $unit holds the line, else the check names the line it lacks.
      has() { grep -qxF "$1" "$unit" || { echo "$unit lacks $1"; exit 1; }; }
      for units in "$systemWideUnits" "$perUserUnits"; do
        unit="$units/steno.service"
        cat "$unit"
        has 'TimeoutStopSec=20s'
        has 'Type=exec'
        has 'Slice=app.slice'
        # A switch leaves a running Steno alone.
        has 'X-RestartIfChanged=false'
        has 'X-StopOnRemoval=false'
        has 'Environment="STENO_LOGIN_ITEM=managed"'
        has 'PartOf=graphical-session.target'
        if grep -q '^Environment="PATH=' "$unit"; then echo "steno.service sets PATH"; exit 1; fi
        test -e "$units/graphical-session.target.wants/steno.service" \
          || { echo "graphical-session.target does not want steno.service"; exit 1; }
        while IFS=$'\t' read -r below source; do
          [ -n "$below" ] || continue
          test -e "$units/$below" || { echo "the user units lack $below"; exit 1; }
        done <<< "$userUnitFiles"
      done
      unit="$systemWideUnits/steno.service"
      has 'ExecStart=/run/current-system/sw/bin/steno-desktop'
      has 'Environment="STENO_HANDOVER_PORT=${toString defaultPort}"'
      unit="$perUserUnits/steno.service"
      has 'ExecStart=/etc/profiles/per-user/%u/bin/steno-desktop'
      has 'ConditionPathExists=/etc/profiles/per-user/%u/bin/steno-desktop'
      has 'Environment="STENO_HANDOVER_PORT=23900"'
      touch $out
    '';
}
