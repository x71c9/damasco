# Two installations of alogeno in a NixOS virtual machine: both answer on
# their port and pass the checks of the sandbox inside their service; the
# owner's link signs in, and one installation serves two sites, each with its
# own preview. A stand-in for Claude Code keeps the test free of unfree
# packages and of the network.
self:
{ pkgs, ... }:
{
  name = "alogeno";

  nodes.machine = {
    imports = [ self.nixosModules.default ];
    virtualisation.memorySize = 2048;
    services.alogeno = {
      enable = true;
      panelDomain = "admin.test";
      claudeCode = pkgs.writeShellScriptBin "claude" ''echo "2.1.0 (Claude Code)"'';
      instances.blog = {
        panelDomain = "admin.blog.test";
        port = 4101;
      };
    };
    environment.systemPackages = [ pkgs.curl ];
  };

  testScript = ''
    import json
    import re

    machine.wait_for_unit("sockets.target")

    def api(method, path, body=None, cookie=None):
        command = f"curl -fsS -X {method} -H 'Host: admin.test' -H 'Content-Type: application/json'"
        if cookie:
            command += f" -H 'Cookie: {cookie}'"
        if body is not None:
            command += f" --data '{json.dumps(body)}'"
        return machine.succeed(f"{command} http://127.0.0.1:4100{path}")

    def status(host):
        return machine.succeed(f"curl -s -o /dev/null -w '%{{http_code}}' -H 'Host: {host}' http://127.0.0.1:4100/").strip()

    with subtest("each installation answers on its own port"):
        assert json.loads(api("GET", "/api/public"))["has_owner"] is False
        machine.succeed("curl -fsS -H 'Host: admin.blog.test' http://127.0.0.1:4101/api/public | grep -F '\"site\":\"admin.blog.test\"'")

    with subtest("the sandbox works inside both services"):
        machine.wait_until_succeeds("journalctl -u alogeno -o cat | grep 'environment ready for the sandbox'")
        machine.wait_until_succeeds("journalctl -u alogeno-blog -o cat | grep 'environment ready for the sandbox'")

    with subtest("the owner's link signs in"):
        printed = machine.succeed("runuser -u alogeno -- alogeno --config /etc/alogeno/alogeno.toml owner-link")
        found = re.search(r"https://admin\.test/setup#(\S+)", printed)
        assert found, printed
        token = found.group(1)
        headers = machine.succeed(
            "curl -fsS -o /dev/null -D - -H 'Host: admin.test' -H 'Content-Type: application/json' "
            + f"--data '{json.dumps({'token': token, 'password': 'a password for the test'})}' "
            + "http://127.0.0.1:4100/api/auth/setup"
        )
        cookie = next(
            line.split(":", 1)[1].split(";", 1)[0].strip()
            for line in headers.splitlines()
            if line.lower().startswith("set-cookie:")
        )

    with subtest("one installation serves two sites, each with its preview"):
        api("POST", "/api/sites", {"name": "Blog", "slug": "blog"}, cookie)
        slugs = sorted(site["slug"] for site in json.loads(api("GET", "/api/sites", cookie=cookie)))
        assert slugs == ["blog", "main"], slugs
        for host in ["main.preview.admin.test", "blog.preview.admin.test"]:
            code = status(host)
            assert code != "421", f"{host}: {code}"
        code = status("other.preview.admin.test")
        assert code == "421", f"other.preview.admin.test: {code}"

    with subtest("updates come from the flake"):
        refusal = machine.fail("alogeno update 2>&1")
        assert "installed with Nix" in refusal, refusal
  '';
}
