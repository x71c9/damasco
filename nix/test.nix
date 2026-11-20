# Two installations of alogeno in a NixOS virtual machine: both answer on
# their port, both pass the checks of the sandbox inside their service, and
# the owner's link works from the command line. A stand-in for Claude Code
# keeps the test free of unfree packages and of the network.
self:
{ pkgs, ... }:
{
  name = "alogeno";

  nodes.machine = {
    imports = [ self.nixosModules.default ];
    virtualisation.memorySize = 2048;
    services.alogeno = {
      enable = true;
      domain = "example.com";
      claudeCode = pkgs.writeShellScriptBin "claude" ''echo "2.1.0 (Claude Code)"'';
      instances.blog = {
        domain = "blog.example.com";
        port = 4101;
      };
    };
    environment.systemPackages = [ pkgs.curl ];
  };

  testScript = ''
    machine.wait_for_unit("sockets.target")

    with subtest("each installation answers on its own port"):
        machine.succeed("curl -fsS -H 'Host: panel.example.com' http://127.0.0.1:4100/api/public | grep -F '\"site\":\"panel.example.com\"'")
        machine.succeed("curl -fsS -H 'Host: panel.blog.example.com' http://127.0.0.1:4101/api/public | grep -F '\"site\":\"panel.blog.example.com\"'")

    with subtest("the sandbox works inside both services"):
        machine.wait_until_succeeds("journalctl -u alogeno -o cat | grep 'environment ready for the sandbox'")
        machine.wait_until_succeeds("journalctl -u alogeno-blog -o cat | grep 'environment ready for the sandbox'")

    with subtest("the owner's link"):
        machine.succeed("runuser -u alogeno -- alogeno --config /etc/alogeno/alogeno.toml owner-link | grep -F 'https://panel.example.com/setup#'")

    with subtest("updates come from the flake"):
        refusal = machine.fail("alogeno update 2>&1")
        assert "installed with Nix" in refusal, refusal
  '';
}
