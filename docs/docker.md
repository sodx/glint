# Docker and remote machines

Code in a container has no global `glint()`, and 127.0.0.1 there is the container itself. Run
`glint docker` in the project folder: it writes a copy of the SDKs to `~/.glint/sdk/container` and
prints what to add to the service in `docker-compose.yml`:

```yaml
    volumes:
      - /Users/me/.glint/sdk/container:/opt/.glint/sdk:ro
    environment:
      GLINT_URL: http://host.docker.internal:23600
      GLINT_PROJECT: shop
      GLINT_PATH_MAP: /var/www/html:/Users/me/shop
      PHP_INI_SCAN_DIR: ":/opt/.glint/sdk/php/conf.d"
    extra_hosts:
      - host.docker.internal:host-gateway
```

Then `glint()` works in PHP, Python and Node in the container, and uncaught errors arrive as they
do on the Mac. `GLINT_PROJECT` names the git repository, which a path like `/var/www/html` does
not tell. `GLINT_PATH_MAP` maps the container folder to this one, so stack frames and Open in
editor point at the files here. `--run` prints `docker run` flags and `--devcontainer` a
devcontainer.json snippet. Inside a container the SDKs try `http://host.docker.internal:23600`
when `GLINT_URL` is not set.

Docker Desktop on a Mac passes `host.docker.internal` to the Mac's 127.0.0.1. On Linux the
container reaches the Docker bridge instead: set `remote_addr = "172.17.0.1:23600"` in
`~/.glint/config.toml` and restart glintd. That listener only takes events; it cannot be used to
read them.

Over SSH, keep glintd on the Mac and open a reverse tunnel, `ssh -R 23600:127.0.0.1:23600 devbox`
(or `RemoteForward 23600 127.0.0.1:23600` in `~/.ssh/config`, which VS Code Remote SSH also
reads). `glint docker --ssh devbox` prints how to copy the SDK there and what to put in the remote
shell profile.

[Back to the README](../README.md)
