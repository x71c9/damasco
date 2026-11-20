# services.alogeno: alogeno on NixOS. The default installation and the named
# ones get the names and paths install.sh gives them, so the documentation
# applies as it is:
#
#   services.alogeno            alogeno.service, user alogeno,
#                               /etc/alogeno/alogeno.toml, /var/lib/alogeno
#   services.alogeno.instances.blog
#                               alogeno-blog.service, user alogeno-blog,
#                               /etc/alogeno-blog/alogeno.toml, /var/lib/alogeno-blog
#
# The service settings follow packaging/systemd/alogeno.service; a change
# there belongs here too. Node.js and Claude Code come from nixpkgs, since
# the builds alogeno downloads expect a dynamic loader NixOS does not have;
# Codex, a static binary, is still downloaded by alogeno. Updates come from
# the flake input, not from the panel.
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

  cfg = config.services.alogeno;
  toml = pkgs.formats.toml { };

  siteOptions = defaultPort: {
    domain = mkOption {
      type = types.str;
      example = "example.com";
      description = "Domain of the installation. The panel answers on panel.<domain>, the preview of each site on <slug>.preview.panel.<domain>.";
    };
    port = mkOption {
      type = if defaultPort == null then types.nullOr types.port else types.port;
      default = defaultPort;
      description = "Local port on 127.0.0.1 the reverse proxy forwards the three hostnames to.";
    };
    settings = mkOption {
      type = toml.type;
      default = { };
      example = {
        limits.agent_memory = "1536M";
        sandbox.agent_allow = [ "github.com" ];
      };
      description = "Further keys of alogeno.toml, merged over the generated ones: [proxy], [limits], [sandbox], or [hosts] to change the hostnames. See docs/install.md.";
    };
  };

  # Every installation by its name, which is also its unit, user and the
  # name of its directories.
  installations =
    lib.optionalAttrs cfg.enable {
      alogeno = {
        inherit (cfg) domain port settings;
      };
    }
    // lib.mapAttrs' (name: site: lib.nameValuePair "alogeno-${name}" site) cfg.instances;

  configFile =
    name: site:
    toml.generate "${name}.toml" (
      lib.recursiveUpdate {
        listen = "127.0.0.1:${toString site.port}";
        data_dir = "/var/lib/${name}";
        hosts.panel = "panel.${site.domain}";
        runtimes = {
          node = lib.getExe cfg.nodejs;
          claude = lib.getExe cfg.claudeCode;
        };
      } site.settings
    );
in
{
  options.services.alogeno = {
    enable = mkEnableOption "alogeno, the default installation";

    package = mkOption {
      type = types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.alogeno;
      defaultText = lib.literalExpression "alogeno.packages.\${system}.alogeno";
      description = "The alogeno package.";
    };

    nodejs = mkPackageOption pkgs "nodejs_26" { };

    claudeCode = mkPackageOption pkgs "claude-code" {
      extraDescription = "Claude Code is unfree: nixpkgs.config.allowUnfree, or an allowUnfreePredicate that admits claude-code.";
    };

    instances = mkOption {
      type = types.attrsOf (types.submodule { options = siteOptions null; });
      default = { };
      example = lib.literalExpression ''
        {
          blog = { domain = "blog.example.com"; port = 4101; };
        }
      '';
      description = "Installations besides the default one, each with its own site, data, user and port.";
    };
  }
  // siteOptions 4100;

  config = mkIf (installations != { }) {
    assertions = [
      {
        assertion = lib.all (site: site.port != null) (lib.attrValues installations);
        message = "services.alogeno.instances.<name>.port: every installation needs its own port.";
      }
      {
        assertion =
          let
            ports = map (site: site.port) (lib.attrValues installations);
          in
          lib.length ports == lib.length (lib.unique ports);
        message = "services.alogeno: two installations use the same port.";
      }
    ];

    # alogeno on the PATH, for owner-link and check.
    environment.systemPackages = [ cfg.package ];

    environment.etc = lib.mapAttrs' (
      name: site: lib.nameValuePair "${name}/alogeno.toml" { source = configFile name site; }
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
      documentation = [ "https://github.com/x71c9/alogeno" ];
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
        ExecStart = "${lib.getExe cfg.package} --config /etc/${name}/alogeno.toml serve";
        Restart = "always";
        RestartSec = "2s";
        TimeoutStopSec = "30s";

        StateDirectory = name;
        StateDirectoryMode = "0700";
        RuntimeDirectory = name;
        RuntimeDirectoryMode = "0700";
        UMask = "0077";

        # A cgroup subtree of its own for the limits of the sandbox; alogeno
        # itself sits in the "main" leaf.
        Delegate = true;
        DelegateSubgroup = "main";
        OOMScoreAdjust = -100;

        # As alogeno.service: no ProtectKernelLogs=, ProtectKernelTunables=,
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
