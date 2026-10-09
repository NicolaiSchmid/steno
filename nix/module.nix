# `programs.steno`: Steno on NixOS. The package, launch at login as a
# systemd user service, PipeWire, and GNOME Keyring as the Secret Service
# where no other Secret Service or SSH agent runs. Item X7 of
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
        The session's `STENO_LOGIN_ITEM=managed` applies to every user, so
        list every user who runs Steno.
      '';
    };

    launchAtLogin = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = ''
        Start Steno with the graphical session, as the systemd user service
        `steno.service`, which sets `STENO_LOGIN_ITEM=managed`: the app
        then leaves launch at login to the system and writes no
        autostart entry of its own. A rebuild never restarts or stops
        `steno.service`, whatever changed; the new version starts at the
        next login, or at the next launch after quitting. One that changes
        PipeWire's units, as most nixpkgs bumps do, restarts PipeWire: a
        recording in progress then reconnects with a second or two of
        silence, and if PipeWire is not back within about 2 s, the
        recording ends and what was recorded is saved. Turning the module
        or this option off leaves a running Steno until it quits or the
        session ends. Needs a
        session that reaches `graphical-session.target`: GNOME, Plasma, or
        Hyprland with `programs.hyprland.withUWSM`.
      '';
    };

    inhibitDelayMaxSec = lib.mkOption {
      type = lib.types.nullOr lib.types.ints.positive;
      default = null;
      example = 15;
      description = ''
        Sets logind's `InhibitDelayMaxSec` (systemd's default is 5 s) so
        that a reboot or a power-off waits for Steno to save a recording in
        progress. `null` leaves logind's setting alone.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    environment.systemPackages = lib.mkIf (cfg.users == []) [cfg.package];
    users.users = lib.genAttrs cfg.users (_: {packages = [cfg.package];});

    # The package's stop timeout drop-ins (P5) for the unit the XDG autostart
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
      # A switch that changes or removes the unit (a nixpkgs bump changes
      # its LOCALE_ARCHIVE and TZDIR) leaves a running Steno alone instead
      # of stopping a recording; the new unit applies at the next login.
      restartIfChanged = false;
      unitConfig.X-StopOnRemoval = false;
      serviceConfig = {
        Type = "exec";
        ExecStart = executable;
        Slice = "app.slice";
        # The save of a recording in progress at logout (P5 of
        # .plans/2026-10-07-stable-promotion.md).
        TimeoutStopSec = "20s";
      };
    };
    # A Steno started from the launcher in a session that began after the
    # switch also agrees that the service owns launch at login.
    environment.sessionVariables = lib.mkIf cfg.launchAtLogin {STENO_LOGIN_ITEM = "managed";};

    # Capture is native PipeWire.
    services.pipewire.enable = lib.mkDefault true;
    security.rtkit.enable = lib.mkDefault true;

    # GNOME Keyring as the Secret Service for the app's secrets (without
    # one they stay in a 0600 file). Not where another one runs (Plasma's
    # KWallet, pass-secret-service), and not beside another SSH agent,
    # because the keyring brings gcr's: nixpkgs refuses it next to
    # `programs.ssh.startAgent`, and next to GnuPG's SSH support gcr's
    # socket takes the session's SSH_AUTH_SOCK, so SSH through gpg-agent
    # stops working. Never set to false, so a desktop's own `mkDefault true`
    # does not conflict.
    services.gnome.gnome-keyring.enable = lib.mkIf (!(
      config.services.desktopManager.plasma6.enable
      || config.services.passSecretService.enable
      || config.programs.ssh.startAgent
      || config.programs.gnupg.agent.enableSSHSupport
    )) (lib.mkDefault true);

    services.logind.settings.Login = lib.mkIf (cfg.inhibitDelayMaxSec != null) {
      InhibitDelayMaxSec = cfg.inhibitDelayMaxSec;
    };
  };
}
