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

    local myshell=/bin/fish

    sed -i "s|^$user:.*|$user:x:$uid:$gid::/home/$user:$myshell|" /etc/passwd
    echo "$user:!x:::::::"                   >> /etc/shadow

    # skeleton HOME
    mkdir -p  /home/$user
    chmod -R 755 /home/$user
    chown -R "$uid:$gid" /home/$user

    #   XDG dirs **before** fish starts so it can write history, etc.
    install -d -m 700  \
        /home/$user/.config \
        /home/$user/.local/share \
        /home/$user/.cache \
        /home/$user/.ssh

    chown "$uid:$gid" \
        /home/$user/.config \
        /home/$user/.local/share \
        /home/$user/.cache \
        /home/$user/.ssh

    # bring over useful root files but NOT its fish config
    need rsync
    rsync -a --chown=$uid:$gid --exclude '.config/fish*' \
        /root/ /home/$user/ || true

    chmod 1777 /tmp                          # sticky-bit temp

    #  after the XDG-dir block, before the EOF heredoc
    install -Dm644 /etc/container-skel/config.fish \
                   /home/$user/.config/fish/config.fish
    touch /home/$user/.config/fish/fish_variables
    chown -R "$uid:$gid" /home/$user
    chmod -R 755 /home/$user
    chmod u+w /home/$user
}

##############################################################################
# 2. main startup logic
##############################################################################
(( EUID == 0 )) || die "please run as root"

# If all three env vars are set, create the user
if [[ -n "${CREATE_USER:-}" && -n "${CREATE_UID:-}" && -n "${CREATE_GID:-}" ]]; then
    create_user "$CREATE_USER" "$CREATE_UID" "$CREATE_GID"
    DEV_USER=$CREATE_USER
    DEV_UID=$CREATE_UID
    DEV_GID=$CREATE_GID
else
    DEV_USER=root
    DEV_UID=0
    DEV_GID=0
fi

##############################################################################
# 3. build users
##############################################################################

# still as root inside the container, create a group for the build users
echo "nixbld:x:30000:" >> /etc/group
echo "nixbld:x::"      >> /etc/gshadow

# Detect CPU count (see table above)
cpus=$(command -v nproc >/dev/null 2>&1 && nproc || getconf _NPROCESSORS_ONLN)

# Ensure the dummy home & shell exist
mkdir -p /var/empty
DUMMY_SHELL=/bin/nologin         # use whatever exists in the image
[ -x "$DUMMY_SHELL" ] || DUMMY_SHELL=/bin/false

members=()

for i in $(seq 1 "$cpus"); do
  muid=$((30000 + i))
  mname="nixbld$i"

  # Skip if we already created the user (makes the script idempotent)
  if ! getent passwd "$mname" >/dev/null; then
    printf '%s:x:%d:30000:Nix build user %d:/var/empty:%s\n' \
           "$mname" "$muid" "$i" "$DUMMY_SHELL" >> /etc/passwd
  fi

  members+=("$mname")
done

# Create or overwrite the group + gshadow line with the member list
member_list=$(IFS=, ; echo "${members[*]}")   # join array with commas

# Remove a possibly-existing (empty) nixbld line first (safe to re-run)
grep -v '^nixbld:' /etc/group   > /etc/group.new
grep -v '^nixbld:' /etc/gshadow > /etc/gshadow.new 2>/dev/null || true

echo "nixbld:x:30000:${member_list}" >> /etc/group.new
echo "nixbld:!:${member_list}:"      >> /etc/gshadow.new 2>/dev/null || true

mv /etc/group.new   /etc/group
mv /etc/gshadow.new /etc/gshadow 2>/dev/null || true

# Start the nix-daemon if not running
if ! pgrep -x nix-daemon >/dev/null ; then
    PATH=/nix/var/nix/profiles/default/bin:$PATH \
        /bin/nix-daemon --daemon &
fi

##############################################################################
# 3.5. Optional dropbear auto-start
##############################################################################
DROPBEAR_STATUS='autorun not configured — run "start-dropbear" to start manually'

if [[ "${DROPBEAR_ENABLE:-0}" == "1" ]]; then
  SSH_DIR="/home/$DEV_USER/.ssh"
  AUTH_KEYS="$SSH_DIR/authorized_keys"
  RSA_KEY="$SSH_DIR/dropbear_rsa_host_key"
  ED25519_KEY="$SSH_DIR/dropbear_ed25519_host_key"
  DROPBEAR_PORT="${DROPBEAR_PORT:-2223}"

  mkdir -p "$SSH_DIR"
  chmod 700 "$SSH_DIR"
  chown "$DEV_UID:$DEV_GID" "$SSH_DIR"

  if [[ -n "${AUTHORIZED_KEYS_B64:-}" ]]; then
    echo "$AUTHORIZED_KEYS_B64" | base64 -d > "$AUTH_KEYS"
    chmod 600 "$AUTH_KEYS"
    chown "$DEV_UID:$DEV_GID" "$AUTH_KEYS"
  else
    DROPBEAR_STATUS="❌ authorized_keys missing (env var AUTHORIZED_KEYS_B64 not set)"
  fi

  if [[ ! -f "$RSA_KEY" ]]; then
    dropbearkey -t rsa -f "$RSA_KEY" > /dev/null
  fi

  if [[ ! -f "$ED25519_KEY" ]]; then
    dropbearkey -t ed25519 -f "$ED25519_KEY" > /dev/null
  fi

  # Run dropbear detached and set up clean flags
  if [[ -f "$AUTH_KEYS" ]]; then
    dropbear -E -a \
      -r "$RSA_KEY" \
      -r "$ED25519_KEY" \
      -p "0.0.0.0:$DROPBEAR_PORT" \
      -P "$SSH_DIR/dropbear.pid" &

    sleep 1  # give it a moment
    if pgrep -x dropbear >/dev/null 2>&1; then
      DROPBEAR_STATUS="running on port $DROPBEAR_PORT"
    else
      DROPBEAR_STATUS="❌ failed to start (check logs)"
    fi
  fi
fi

##############################################################################
# 4. summary banner
##############################################################################
echo "───────────────────────────────────────────────────────────────────────────────"
echo " 🚀  Container ready!"
echo
echo " • User ............. $DEV_USER  (uid=$DEV_UID / gid=$DEV_GID)"
echo " • SSH server ....... $DROPBEAR_STATUS"

if mountpoint -q /workspace 2>/dev/null; then
  echo " • Volume mount ..... /workspace is mounted"
else
  echo " • Volume mount ..... none detected"
fi

echo
echo " ✅  Environment configuration complete"
echo " 🆘  Type 'polar-help' for container usage instructions"
echo "───────────────────────────────────────────────────────────────────────────────"

##############################################################################
# 5.  hand control to the user shell
##############################################################################

HOME=/home/$DEV_USER \
  LOGNAME=$DEV_USER \
  SHELL=/bin/fish \
  USER=$DEV_USER \
  XDG_CACHE_HOME=/home/$DEV_USER/.cache \
  XDG_CONFIG_HOME=/home/$DEV_USER/.config \
  XDG_DATA_HOME=/home/$DEV_USER/.local/share \
  chroot --userspec="$DEV_UID:$DEV_GID" / /bin/fish -l
