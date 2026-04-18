# src/containers/git-server/package.nix
#
# Packages the polar-git-server container image.
#
# This is a local development git HTTP server. It serves local git working
# trees over HTTP using nginx + fcgiwrap + git-http-backend. It is NOT
# deployed into the cluster — it runs on the host via podman.
#
# Repositories are mounted into the container at runtime. The entrypoint
# starts fcgiwrap and nginx, then blocks until the container is stopped.
#
# See README.md for usage and Justfile for the podman run command.

{ pkgs
, nix-container-lib
, inputs
, system
}:

let

  # nginx configuration for git-http-backend via fcgiwrap.
  #
  # Repos are mounted at /srv/git/<name>.git and served at /<name>.git/
  #
  # PATH_INFO must include the repo name when using GIT_PROJECT_ROOT.
  # We pass $uri (the full request path) as PATH_INFO and set
  # GIT_PROJECT_ROOT to /srv/git so git-http-backend resolves:
  #   /srv/git  +  /Polar.git/info/refs  →  /srv/git/Polar.git
  #
  # GIT_HTTP_EXPORT_ALL bypasses git-daemon-export-ok so normal
  # working trees (not bare repos) are served correctly.
  nginxConf = pkgs.writeText "nginx.conf" ''
    worker_processes 1;
    error_log /dev/stderr warn;
    pid /tmp/nginx.pid;

    events {
      worker_connections 64;
    }

    http {
      access_log /dev/stdout;
      client_body_temp_path /tmp/nginx-client-body;
      proxy_temp_path       /tmp/nginx-proxy;
      fastcgi_temp_path     /tmp/nginx-fastcgi;
      uwsgi_temp_path       /tmp/nginx-uwsgi;
      scgi_temp_path        /tmp/nginx-scgi;

      server {
        listen 3000;
        server_name localhost;

        location ~ ^/.+\.git(/.*)?$ {
          fastcgi_pass  unix:/tmp/fcgiwrap.sock;
          fastcgi_param SCRIPT_FILENAME     ${pkgs.git}/libexec/git-core/git-http-backend;
          fastcgi_param PATH_INFO           $uri;
          fastcgi_param GIT_PROJECT_ROOT    /srv/git;
          fastcgi_param GIT_HTTP_EXPORT_ALL "";
          fastcgi_param REQUEST_METHOD      $request_method;
          fastcgi_param QUERY_STRING        $query_string;
          fastcgi_param CONTENT_TYPE        $content_type;
          fastcgi_param CONTENT_LENGTH      $content_length;
          fastcgi_param SERVER_PROTOCOL     $server_protocol;
          fastcgi_param REQUEST_URI         $request_uri;
          fastcgi_param REMOTE_ADDR         $remote_addr;
          fastcgi_read_timeout 300s;
        }

        location / {
          return 404 "not found\n";
        }
      }
    }
  '';

  # Entrypoint: start fcgiwrap in the background, then exec nginx in the
  # foreground so the container lifecycle is tied to nginx.
  gitServerEntrypoint = pkgs.writeShellApplication {
    name = "git-server-entrypoint";

    runtimeInputs = [
      pkgs.fcgiwrap
      pkgs.nginx
      pkgs.git
      pkgs.coreutils
    ];

    text = ''
      set -euo pipefail

      # Allow git-http-backend to serve repos regardless of directory ownership.
      # Required when the mounted repo was created by a different uid.
      export GIT_CONFIG_COUNT=1
      export GIT_CONFIG_KEY_0=safe.directory
      export GIT_CONFIG_VALUE_0='*'

      # nginx needs writable temp dirs — use /tmp (provided via --tmpfs in podman run)
      mkdir -p \
        /tmp/nginx-client-body \
        /tmp/nginx-proxy \
        /tmp/nginx-fastcgi \
        /tmp/nginx-uwsgi \
        /tmp/nginx-scgi

      echo "[git-server] starting fcgiwrap..."
      fcgiwrap -s unix:/tmp/fcgiwrap.sock &

      # Wait for the socket to appear before starting nginx
      for i in $(seq 1 10); do
        [ -S /tmp/fcgiwrap.sock ] && break
        echo "[git-server] waiting for fcgiwrap socket... ($i/10)"
        sleep 0.5
      done

      if [ ! -S /tmp/fcgiwrap.sock ]; then
        echo "[git-server] ERROR: fcgiwrap socket never appeared" >&2
        exit 1
      fi

      echo "[git-server] starting nginx on port 3000..."
      echo "[git-server] serving repos from /srv/git/"
      echo "[git-server] clone URLs: http://<host-ip>:3000/<repo-name>.git"

      exec nginx -c ${nginxConf} -g "daemon off;"
    '';
  };

  gitServerContainer = nix-container-lib.lib.${system}.mkContainer {
    inherit system pkgs inputs;
    configNixPath    = ./container.nix;
    extraDerivations = [ gitServerEntrypoint ];
  };

in
{
  inherit gitServerEntrypoint;
  image = gitServerContainer.image;
}
