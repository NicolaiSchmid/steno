# `programs.steno`: Steno on NixOS. The package, launch at login as a
# systemd user service, PipeWire and a Secret Service provider. Item X7 of
# .plans/2026-10-07-stable-promotion.md. The handover port in the firewall
# is X4's and not here yet.
{packages}: {
  config,
  lib,
  pkgs,
  ...
}: let
  cfg = config.programs.steno;
  system = pkgs.stdenv.hostPlatform.system;
  # A path that stays the same across upgrades and outlives a garbage
  # collection, never a store path: the profile the module installs into.
  # `%u` is the user the user manager runs for.
  executable =
    if cfg.users == []
    then "/run/current-system/sw/bin/steno-desktop"
    else "/etc/profiles/per-user/%u/bin/steno-desktop";
in {
  options.programs.steno = {
    enable = lib.mkEnableOption "Steno, the bot-free meeting recorder";

    package = lib.mkOption {
      type = lib.types.package;
      default = packages.${system}.steno or (throw "programs.steno: no Steno package for ${system}");
      defaultText = lib.literalExpression "steno.packages.\${system}.steno";
      description = "The Steno package to install.";
    };

    users = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [];
      example = ["alice"];
      description = ''
        Install Steno for these users only (their `users.users.<name>.packages`)
        instead of system-wide. Launch at login then starts it only for them.
      '';
    };

    launchAtLogin = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = ''
        Start Steno with the graphical session, as the systemd user service
        `steno.service`. The app then leaves launch at login to the system
        (`STENO_LOGIN_ITEM=managed`) and writes no autostart entry of its
        own. Needs a session that reaches `graphical-session.target`: GNOME,
        Plasma, or Hyprland with `programs.hyprland.withUWSM`.
      '';
    };

    inhibitDelayMaxSec = lib.mkOption {
      type = lib.types.nullOr lib.types.ints.positive;
      default = null;
      example = 15;
      description = ''
        Raise logind's `InhibitDelayMaxSec` (5 s unless set) so that a
        reboot or a power-off waits long enough for Steno to save a
        recording in progress. Off unless set.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    environment.systemPackages = lib.mkIf (cfg.users == []) [cfg.package];
    users.users = lib.genAttrs cfg.users (_: {packages = [cfg.package];});

    # The package's stop timeout drop-ins for the unit the XDG autostart
    # generator makes and for GNOME's app scope: `systemd.packages` links
    # lib/systemd/user/ into the user units as well.
    systemd.packages = [cfg.package];

    systemd.user.services.steno = lib.mkIf cfg.launchAtLogin {
      description = "Steno meeting recorder";
      wantedBy = ["graphical-session.target"];
      partOf = ["graphical-session.target"];
      after = ["graphical-session.target"];
      # For a per-user install: users without Steno skip the unit.
      unitConfig.ConditionPathExists = executable;
      # No PATH of the module's own: the app keeps the session's, which
      # names what it opens files and links with.
      enableDefaultPath = false;
      environment.STENO_LOGIN_ITEM = "managed";
      serviceConfig = {
        Type = "exec";
        ExecStart = executable;
        Slice = "app.slice";
        # The save of a recording in progress at logout (P5 of the plan).
        TimeoutStopSec = "20s";
      };
    };
    # A Steno started from the launcher in the same session agrees that
    # the service owns launch at login.
    environment.sessionVariables = lib.mkIf cfg.launchAtLogin {STENO_LOGIN_ITEM = "managed";};

    # Capture is native PipeWire.
    services.pipewire.enable = lib.mkDefault true;
    security.rtkit.enable = lib.mkDefault true;

    # A Secret Service for the app's secrets. Plasma brings KWallet's;
    # anything else gets GNOME Keyring unless the configuration says
    # otherwise.
    services.gnome.gnome-keyring.enable =
      lib.mkDefault (!config.services.desktopManager.plasma6.enable);

    services.logind.settings.Login = lib.mkIf (cfg.inhibitDelayMaxSec != null) {
      InhibitDelayMaxSec = cfg.inhibitDelayMaxSec;
    };
  };
}
