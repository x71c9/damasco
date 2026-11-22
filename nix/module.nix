# services.damasco: damasco on NixOS. The default installation and the named
# ones get the names and paths install.sh gives them, so the documentation
# applies as it is:
#
#   services.damasco            damasco.service, user damasco,
#                               /etc/damasco/damasco.toml, /var/lib/damasco
#   services.damasco.instances.blog
#                               damasco-blog.service, user damasco-blog,
#                               /etc/damasco-blog/damasco.toml, /var/lib/damasco-blog
#
# The service settings follow packaging/systemd/damasco.service; a change
# there belongs here too. Node.js and Claude Code come from nixpkgs, since
# the builds damasco downloads expect a dynamic loader NixOS does not have:
# Node.js from the system's, Claude Code from the one this flake locks, in
# the version damasco is tested with (a stable channel keeps one too old for
# the artifacts). Codex, a static binary, is still downloaded by damasco.
# Updates come from the flake input, not from the panel.
self:
{
  config,
  lib,
  pkgs,
  ...
}:

let
  inherit (lib)
    mkEnableOption
    mkIf
    mkOption
    mkPackageOption
    types
    ;

  cfg = config.services.damasco;
  toml = pkgs.formats.toml { };

  # The nixpkgs this flake locks, under the system's own configuration: what
  # it says of unfree packages holds for Claude Code.
  pinned = import self.inputs.nixpkgs {
    inherit (pkgs.stdenv.hostPlatform) system;
    inherit (pkgs) config;
  };

  siteOptions = defaultPort: {
    panelDomain = mkOption {
      type = types.str;
      example = "panel.example.com";
      description = "Domain of the panel. The preview of each site answers on <slug>.preview.<panelDomain>.";
    };
    port = mkOption {
      type = if defaultPort == null then types.nullOr types.port else types.port;
      default = defaultPort;
      description = "Local port on 127.0.0.1 the reverse proxy forwards the panel, the previews and the sites to.";
    };
    askPort = mkOption {
      type = types.nullOr types.port;
      default = null;
      example = 4099;
      description = "Local port where the reverse proxy asks whether a name is served here, before getting its certificate (Caddy's on-demand TLS): domains added in the panel then work without touching the proxy. One installation per machine at most; with null, every name goes into the proxy by hand.";
    };
    settings = mkOption {
      type = toml.type;
      default = { };
      example = {
        limits.agent_memory = "1536M";
        sandbox.agent_allow = [ "github.com" ];
      };
      description = "Further keys of damasco.toml, merged over the generated ones: [proxy], [limits], [sandbox], or [hosts] to change the hostnames. See docs/install.md.";
    };
  };

  # Every installation by its name, which is also its unit, user and the
  # name of its directories.
  installations =
    lib.optionalAttrs cfg.enable {
      damasco = {
        inherit (cfg)
          panelDomain
          port
          askPort
          settings
          ;
      };
    }
    // lib.mapAttrs' (name: site: lib.nameValuePair "damasco-${name}" site) cfg.instances;

  # The installations the proxy asks: one at most.
  asking = lib.filterAttrs (_: site: site.askPort != null) installations;
  asked = lib.head (lib.attrValues asking);

  configFile =
    name: site:
    toml.generate "${name}.toml" (
      lib.recursiveUpdate (
        {
          listen = "127.0.0.1:${toString site.port}";
          data_dir = "/var/lib/${name}";
          hosts.panel = site.panelDomain;
          runtimes = {
            node = lib.getExe cfg.nodejs;
            claude = lib.getExe cfg.claudeCode;
          };
        }
        // lib.optionalAttrs (site.askPort != null) {
          domains.ask_listen = "127.0.0.1:${toString site.askPort}";
        }
      ) site.settings
    );
in
{
  options.services.damasco = {
    enable = mkEnableOption "damasco, the default installation";

    package = mkOption {
      type = types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.damasco;
      defaultText = lib.literalExpression "damasco.packages.\${system}.damasco";
      description = "The damasco package.";
    };

    nodejs = mkPackageOption pkgs "nodejs_26" { };

    claudeCode = mkOption {
      type = types.package;
      default = pinned.claude-code;
      defaultText = lib.literalExpression "damasco.inputs.nixpkgs: claude-code";
      description = "Claude Code, by default from the nixpkgs of the damasco flake rather than the system's, in the version damasco is tested with. It is unfree: nixpkgs.config.allowUnfree, or an allowUnfreePredicate that admits claude-code.";
    };

    instances = mkOption {
      type = types.attrsOf (types.submodule { options = siteOptions null; });
      default = { };
      example = lib.literalExpression ''
        {
          blog = { panelDomain = "panel.blog.example.com"; port = 4101; };
        }
      '';
      description = "Installations besides the default one, each with its own sites, data, user and port.";
    };

    caddy.enable = mkEnableOption ''
      Caddy in front of the installation with askPort: one site block serves
      the panel, the previews and every domain added in the panel, with a
      certificate obtained on the first visit of each name after damasco
      confirms it serves it. Installations without askPort go into
      services.caddy.virtualHosts by hand, and take precedence over this
      block for their names'';
  }
  // siteOptions 4100;

  config = mkIf (installations != { }) {
    services.caddy = mkIf cfg.caddy.enable {
      enable = true;
      globalConfig = ''
        on_demand_tls {
        	ask http://127.0.0.1:${toString asked.askPort}/ask
        }
      '';
      virtualHosts."https://".extraConfig = ''
        tls {
        	on_demand
        }
        reverse_proxy 127.0.0.1:${toString asked.port}
      '';
    };

    assertions = [
      {
        assertion = lib.length (lib.attrNames asking) <= 1;
        message = "services.damasco: askPort is set on ${lib.concatStringsSep ", " (lib.attrNames asking)}; the reverse proxy asks one installation per machine, the others go into it by hand.";
      }
      {
        assertion = !cfg.caddy.enable || asking != { };
        message = "services.damasco.caddy.enable needs the installation the proxy asks: set askPort on one installation.";
      }
      {
        assertion =
          let
            ports = lib.concatMap (site: [ site.port ] ++ lib.optional (site.askPort != null) site.askPort) (
              lib.attrValues installations
            );
          in
          lib.length ports == lib.length (lib.unique ports);
        message = "services.damasco: two installations, or an installation and an askPort, use the same port.";
      }
      {
        assertion = lib.all (site: site.port != null) (lib.attrValues installations);
        message = "services.damasco.instances.<name>.port: every installation needs its own port.";
      }
    ];

    # damasco on the PATH, for owner-link and check.
    environment.systemPackages = [ cfg.package ];

    environment.etc = lib.mapAttrs' (
      name: site: lib.nameValuePair "${name}/damasco.toml" { source = configFile name site; }
    ) installations;

    users.users = lib.mapAttrs (name: _: {
      isSystemUser = true;
      group = name;
      home = "/var/lib/${name}";
    }) installations;
    users.groups = lib.mapAttrs (_: _: { }) installations;

    systemd.sockets = lib.mapAttrs (name: site: {
      description = "${name} listening socket";
      wantedBy = [ "sockets.target" ];
      listenStreams = [ "127.0.0.1:${toString site.port}" ];
      socketConfig.NoDelay = true;
    }) installations;

    systemd.services = lib.mapAttrs (name: site: {
      description = "${name}, website builder driven by a coding agent";
      documentation = [ "https://github.com/x71c9/damasco" ];
      wantedBy = [ "multi-user.target" ];
      requires = [ "${name}.socket" ];
      after = [
        "${name}.socket"
        "network-online.target"
      ];
      wants = [ "network-online.target" ];
      # bubblewrap runs the sandbox; git keeps the history of the site.
      path = [
        pkgs.bubblewrap
        pkgs.git
      ];
      restartTriggers = [ (configFile name site) ];
      serviceConfig = {
        Type = "exec";
        User = name;
        Group = name;
        ExecStart = "${lib.getExe cfg.package} --config /etc/${name}/damasco.toml serve";
        Restart = "always";
        RestartSec = "2s";
        TimeoutStopSec = "30s";

        StateDirectory = name;
        StateDirectoryMode = "0700";
        RuntimeDirectory = name;
        RuntimeDirectoryMode = "0700";
        UMask = "0077";

        # A cgroup subtree of its own for the limits of the sandbox; damasco
        # itself sits in the "main" leaf.
        Delegate = true;
        DelegateSubgroup = "main";
        OOMScoreAdjust = -100;

        # As damasco.service: no ProtectKernelLogs=, ProtectKernelTunables=,
        # ProtectHostname= or RestrictSUIDSGID=, which keep bubblewrap from
        # mounting its /proc or setting up its mounts.
        NoNewPrivileges = true;
        ProtectSystem = "strict";
        ProtectHome = true;
        PrivateTmp = true;
        ProtectKernelModules = true;
        ProtectClock = true;
        LockPersonality = true;
        RestrictRealtime = true;
        RestrictAddressFamilies = [
          "AF_UNIX"
          "AF_INET"
          "AF_INET6"
          "AF_NETLINK"
        ];
        LimitNOFILE = 65536;
      };
    }) installations;
  };
}
