# Two installations of damasco in a NixOS virtual machine: both answer on
# their port and pass the checks of the sandbox inside their service; the
# owner's link signs in, and one installation serves several sites, each
# with its preview. The default installation sits behind Caddy, which asks it
# before getting a certificate: the sites' domains work over HTTPS as soon as
# they are added in the panel. Caddy's own CA stands in for Let's Encrypt and
# a stand-in for Claude Code keeps the test free of unfree packages and of
# the network.
self:
{ pkgs, ... }:
{
  name = "damasco";

  nodes.machine = {
    imports = [ self.nixosModules.default ];
    virtualisation.memorySize = 2048;
    services.damasco = {
      enable = true;
      panelDomain = "admin.test";
      askPort = 4099;
      caddy.enable = true;
      claudeCode = pkgs.writeShellScriptBin "claude" ''echo "2.1.0 (Claude Code)"'';
      instances.blog = {
        panelDomain = "admin.blog.test";
        port = 4101;
      };
    };
    services.caddy.globalConfig = ''
      local_certs
      skip_install_trust
    '';
    networking.hosts."127.0.0.1" = [
      "admin.test"
      "a.preview.admin.test"
      "b.preview.admin.test"
      "a.test"
      "www.a.test"
      "b.test"
    ];
    environment.systemPackages = [ pkgs.curl ];
  };

  testScript = ''
    import json
    import re

    machine.wait_for_unit("sockets.target")

    def api(method, path, body=None, cookie=None, extra=""):
        command = f"curl -fsS -X {method} -H 'Host: admin.test' -H 'Content-Type: application/json' {extra}"
        if cookie:
            command += f" -H 'Cookie: {cookie}'"
        if body is not None:
            command += f" --data '{json.dumps(body)}'"
        return machine.succeed(f"{command} http://127.0.0.1:4100{path}")

    def status(command):
        return machine.succeed(f"curl -s -o /dev/null -w '%{{http_code}}' {command}").strip()

    def ask(name):
        return status(f"'http://127.0.0.1:4099/ask?domain={name}'")

    with subtest("each installation answers on its own port"):
        assert json.loads(api("GET", "/api/public"))["has_owner"] is False
        machine.succeed("curl -fsS -H 'Host: admin.blog.test' http://127.0.0.1:4101/api/public | grep -F '\"site\":\"admin.blog.test\"'")

    with subtest("the sandbox works inside both services"):
        machine.wait_until_succeeds("journalctl -u damasco -o cat | grep 'environment ready for the sandbox'")
        machine.wait_until_succeeds("journalctl -u damasco-blog -o cat | grep 'environment ready for the sandbox'")

    with subtest("the owner's link signs in"):
        printed = machine.succeed("runuser -u damasco -- damasco --config /etc/damasco/damasco.toml owner-link")
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

    with subtest("one installation serves several sites, each with its preview"):
        ids = {}
        for slug in ["a", "b"]:
            ids[slug] = json.loads(api("POST", "/api/sites", {"name": slug.upper(), "slug": slug}, cookie))["id"]
        slugs = sorted(site["slug"] for site in json.loads(api("GET", "/api/sites", cookie=cookie)))
        assert slugs == ["a", "b", "main"], slugs
        for host in ["main.preview.admin.test", "a.preview.admin.test"]:
            code = status(f"-H 'Host: {host}' http://127.0.0.1:4100/")
            assert code != "421", f"{host}: {code}"
        code = status("-H 'Host: other.preview.admin.test' http://127.0.0.1:4100/")
        assert code == "421", f"other.preview.admin.test: {code}"

    with subtest("the proxy gets certificates for the served names only"):
        machine.wait_for_unit("caddy.service")
        machine.wait_for_open_port(4099)
        for name in ["admin.test", "a.preview.admin.test"]:
            assert ask(name) == "200", name
        for name in ["x.preview.admin.test", "evil.test", "a.test"]:
            assert ask(name) == "403", name

    with subtest("a domain added in the panel works over HTTPS at once"):
        api("POST", f"/api/sites/{ids['a']}/domains", {"host": "a.test", "www": True}, cookie)
        api("POST", f"/api/sites/{ids['b']}/domains", {"host": "b.test"}, cookie)
        assert ask("a.test") == "200"
        assert ask("www.a.test") == "200"
        instance = machine.succeed("curl -fsSk https://admin.test/_damasco/health").strip()
        assert instance.startswith("ok "), instance
        for host in ["a.test", "b.test", "a.preview.admin.test", "b.preview.admin.test"]:
            answer = machine.succeed(f"curl -fsSk https://{host}/_damasco/health").strip()
            assert answer == instance, f"{host}: {answer}"
            code = status(f"-k https://{host}/")
            assert code not in ["421", "502"], f"{host}: {code}"

    with subtest("the www sends its visitors to the domain"):
        redirect = machine.succeed("curl -sk -o /dev/null -w '%{http_code} %{redirect_url}' https://www.a.test/about").strip()
        assert redirect == "308 https://a.test/about", redirect

    with subtest("a preview cannot act on the panel"):
        code = status(
            f"-k -X POST -H 'Origin: https://a.preview.admin.test' -H 'Cookie: {cookie}' "
            + "-H 'Content-Type: application/json' --data '{\"name\":\"X\",\"slug\":\"x\"}' https://admin.test/api/sites"
        )
        assert code == "403", code

    with subtest("updates come from the flake"):
        refusal = machine.fail("damasco update 2>&1")
        assert "installed with Nix" in refusal, refusal
  '';
}
