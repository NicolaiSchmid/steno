# The Linux checks behind `nix flake check`: the package's layout and
# libraries, and the NixOS module evaluated in two minimal systems (a
# system-wide and a per-user install) down to the user units it generates.
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
    }
  ];
  userUnits = config: config.environment.etc."systemd/user".source;
  installs = config: lib.elem steno config.environment.systemPackages;
in {
  package =
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
      grep -q "$bin/.steno-desktop-wrapped" "$bin/steno-desktop"
      grep -q 'STENO_DISTRIBUTION' "$bin/steno-desktop"
      for elf in .steno-desktop-wrapped steno-speech-sidecar; do
        file -L "$bin/$elf" | grep -q ELF || { echo "$elf is not an ELF binary"; exit 1; }
        if $ldd "$bin/$elf" | grep 'not found'; then echo "$elf misses a library"; exit 1; fi
      done
      # nixpkgs' ONNX Runtime, linked, in both.
      $ldd "$bin/steno-speech-sidecar" | grep -q "$onnxruntime/lib/libonnxruntime.so"
      $ldd "$bin/.steno-desktop-wrapped" | grep -q "$onnxruntime/lib/libonnxruntime.so"

      # The tray opens libayatana-appindicator by this store path.
      test -f "$tray"
      grep -qaF "$tray" "$bin/.steno-desktop-wrapped"

      grep -q '^Exec=steno-desktop %u$' ${steno}/share/applications/steno-desktop.desktop

      while IFS=$'\t' read -r below source; do
        [ -n "$below" ] || continue
        cmp "${steno}/lib/systemd/user/$below" "$src/$source"
        cmp "${steno}/share/systemd/user/$below" "$src/$source"
        echo "ok: lib/systemd/user/$below, also under share/"
      done <<< "$userUnitFiles"
      touch $out
    '';

  module = assert installs systemWide.config;
  assert !(installs perUser.config);
  assert lib.elem steno perUser.config.users.users.alice.packages;
  assert systemWide.config.services.pipewire.enable;
  assert systemWide.config.services.gnome.gnome-keyring.enable;
  assert !(systemWide.config.services.logind.settings.Login ? InhibitDelayMaxSec);
  assert perUser.config.services.logind.settings.Login.InhibitDelayMaxSec == 15;
    pkgs.runCommand "steno-module-check" {
      inherit userUnitFiles;
      systemWideUnits = userUnits systemWide.config;
      perUserUnits = userUnits perUser.config;
    } ''
      set -euo pipefail
      for units in "$systemWideUnits" "$perUserUnits"; do
        unit="$units/steno.service"
        cat "$unit"
        grep -qx 'TimeoutStopSec=20s' "$unit"
        grep -qx 'Environment="STENO_LOGIN_ITEM=managed"' "$unit"
        grep -qx 'PartOf=graphical-session.target' "$unit"
        if grep -q '^Environment="PATH=' "$unit"; then echo "the unit sets PATH"; exit 1; fi
        test -e "$units/graphical-session.target.wants/steno.service"
        while IFS=$'\t' read -r below source; do
          [ -n "$below" ] || continue
          test -e "$units/$below"
        done <<< "$userUnitFiles"
      done
      grep -qx 'ExecStart=/run/current-system/sw/bin/steno-desktop' "$systemWideUnits/steno.service"
      grep -qx 'ExecStart=/etc/profiles/per-user/%u/bin/steno-desktop' "$perUserUnits/steno.service"
      grep -qx 'ConditionPathExists=/etc/profiles/per-user/%u/bin/steno-desktop' "$perUserUnits/steno.service"
      touch $out
    '';
}
