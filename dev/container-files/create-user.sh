#!/usr/bin/env bash
set -euo pipefail

##############################################################################
# 0. tiny helpers
##############################################################################
die() { echo >&2 "error: $*"; exit 1; }
need() { command -v "$1" >/dev/null || die "missing binary: $1"; }

##############################################################################
# 1. create the normal dev user  (original logic, trimmed a little)
##############################################################################
create_user() {
    local user=$1 uid=$2 gid=$3

    [[ -z $user || -z $uid || -z $gid ]] && die "Usage: $0 <user> <uid> <gid>"

    echo "$user:x:$gid:"                     >> /etc/group
    echo "$user:x::"                         >> /etc/gshadow
    echo "$user:x:$uid:$gid::/home/$user:/bin/fish" >> /etc/passwd
    echo "$user:!x:::::::"                   >> /etc/shadow

    # skeleton HOME
    install -d -o "$uid" -g "$gid" -m 755  /home/$user

    #   XDG dirs **before** fish starts so it can write history, etc.
    install -d -o "$uid" -g "$gid" -m 700  \
        /home/$user/.config \
        /home/$user/.local/share \
        /home/$user/.cache \
        /home/$user/.ssh

    # bring over useful root files but NOT its fish config
    need rsync
    rsync -a --chown=$uid:$gid --exclude '.config/fish*' \
        /root/ /home/$user/ || true

    chmod 1777 /tmp                          # sticky‑bit temp

    #  after the XDG‑dir block, before the EOF heredoc
    install -Dm644 /etc/container‑skel/config.fish \
                   /home/$user/.config/fish/config.fish
    chown  "$uid:$gid" /home/$user/.config/fish/config.fish
}

##############################################################################
# 2. start dropbear (key‑auth only, loopback)
##############################################################################
start_dropbear() {
    need dropbear
    need ssh-keygen

    local port=${DROPBEAR_PORT:-2222}
    local db_dir=/etc/dropbear
    install -d -m 700 "$db_dir"

    # host key (quiet)
    [[ -s $db_dir/dropbear_rsa_host_key ]] || \
        ssh-keygen -t rsa -N '' -f "$db_dir/dropbear_rsa_host_key" \
        >/dev/null 2>&1

    # -a  localhost‑only   -R <=3072bit rekey   -E log→stderr
    # run in background; if it fails we abort the script (`set -e`)
    dropbear -a -R -E -p "$port" >/dev/null 2>&1 &

    echo "$port"      # print *only* the port for the banner
}

##############################################################################
# 3. main
##############################################################################
(( EUID == 0 )) || die "please run as root"
[[ $# == 3 ]]   || die "Usage: \$0 <user> <uid> <gid>"

user=$1; uid=$2; gid=$3
create_user "$user" "$uid" "$gid"
port=$(start_dropbear)

cat <<BANNER
───────────────────────────────────────────────────────────────────────────────
 🚀  Container ready!

 • User ............. $user  (uid=$uid / gid=$gid)
 • SSH server ....... dropbear on 127.0.0.1:$port  (🔑 key‑auth only)

 👉  To connect from host (Zed, VS Code Remote‑SSH, etc.):

     # once per container – copy your public key in:
     docker cp ~/.ssh/id_ed25519.pub $(hostname):/home/$user/.ssh/authorized_keys

     # then connect:
     ssh -p $port $user@127.0.0.1

───────────────────────────────────────────────────────────────────────────────
BANNER

##############################################################################
# 4.  hand control to the user shell  (keep the OCI ENV that was set
#     by the image / container‑runtime, just add/override a few names)
##############################################################################

HOME=/home/$user \
  LOGNAME=$user \
  SHELL=/bin/fish \
  USER=$user \
  XDG_CACHE_HOME=/home/$user/.cache \
  XDG_CONFIG_HOME=/home/$user/.config \
  XDG_DATA_HOME=/home/$user/.local/share \
  chroot --userspec="$uid:$gid" / /bin/fish -l   # login‑shell so fish loads config
