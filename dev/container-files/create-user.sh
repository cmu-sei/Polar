#!/usr/bin/env bash
set -euo pipefail

# ─────────────────────────────────────────────────────────────────────────────
# 1. create the normal dev user  (original logic, untouched)
# ─────────────────────────────────────────────────────────────────────────────
create_user_in_container() {
    local username=$1 uid=$2 gid=$3

    [[ -z $username || -z $uid || -z $gid ]] && {
        echo "Usage: $0 username uid gid" ; exit 1; }

    echo "$username:x:$gid:"                >> /etc/group
    echo "$username:x::"                    >> /etc/gshadow
    echo "$username:x:$uid:$gid::/home/$username:/bin/fish" >> /etc/passwd
    echo "$username:!x:::::::"              >> /etc/shadow

    mkdir -p          /home/$username
    cp -R /root/.     /home/$username || true
    chmod 755         /home/$username
    chown -R "$username:$username" /home/$username

    chmod 1777 /tmp   # sticky‑bit temp

    # fish config + XDG dirs
    local cfg=/home/$username/.config/fish/config.fish
    install -D -m 644 /dev/null "$cfg"
    cat >>"$cfg" <<EOF

# ── added by create‑user.sh
set -x HOME            /home/$username
set -x FISH_CONFIG_DIR \$HOME/.config/fish
set -x XDG_DATA_HOME   \$HOME/.local/share
set -x XDG_CONFIG_HOME \$HOME/.config
set -x XDG_CACHE_HOME  \$HOME/.local/share
EOF

    # ────────────────────────────────────────────────────────────────────────
    # 2.  DROPBEAR SSH SET‑UP  (✅ NEW)
    # ────────────────────────────────────────────────────────────────────────
    local DB_PORT=\${DROPBEAR_PORT:-2222}
    local DB_DIR=/etc/dropbear
    local AUTH_KEYS=/home/$username/.ssh/authorized_keys

    # packages must be in the image: dropbear & openssh
    command -v dropbear  >/dev/null
    command -v ssh-keygen >/dev/null

    mkdir -p \$DB_DIR
    [[ -s \$DB_DIR/dropbear_rsa_host_key ]] || \
        ssh-keygen -t rsa -N '' -f \$DB_DIR/dropbear_rsa_host_key

    install -d -o "$username" -g "$username" -m 700 /home/$username/.ssh
    touch      \$AUTH_KEYS
    chown "$username:$username" \$AUTH_KEYS
    chmod 600  \$AUTH_KEYS

    # start dropbear (key‑only, loop‑back)
    dropbear -R -E -F -p 127.0.0.1:\$DB_PORT &
    export DROPBEAR_PID=$!

    # ────────────────────────────────────────────────────────────────────────
    # 3. helpful banner
    # ────────────────────────────────────────────────────────────────────────
    cat <<BANNER

───────────────────────────────────────────────────────────────────────────────
 🚀  Container ready!

 • User ............. $username  (uid=$uid / gid=$gid)
 • SSH server ....... dropbear on 127.0.0.1:\$DB_PORT  (🔑 key‑auth only)

 👉  To connect from host (Zed, VS Code Remote‑SSH, etc.):

     # once per container – copy your public key in:
     docker cp ~/.ssh/id_ed25519.pub <container-id>:${AUTH_KEYS}

     # then connect:
     ssh -p \$DB_PORT $username@127.0.0.1

   (If you use docker‑compose: add   ports: ["127.0.0.1:\$DB_PORT:\$DB_PORT"] )

 Have fun!
───────────────────────────────────────────────────────────────────────────────
BANNER

    # ────────────────────────────────────────────────────────────────────────
    # 4. hand control to the requested command (original)
    # ────────────────────────────────────────────────────────────────────────
    exec chroot --userspec=$uid:$gid / /bin/fish -c "cd /workspace; exec fish"
}

# must run as root
(( EUID == 0 )) || { echo "Please run as root" ; exit 1; }

# args: username uid gid
[[ $# == 3 ]] || { echo "Usage: $0 username uid gid" ; exit 1; }

create_user_in_container "$@"
