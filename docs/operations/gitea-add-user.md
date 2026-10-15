<!--
  ACE Tool

  Copyright 2026 Carnegie Mellon University.

  NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
  FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER
  EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR
  PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE
  MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT,
  TRADEMARK, OR COPYRIGHT INFRINGEMENT.

  Licensed under a MIT (SEI)-style license, please see license.txt or contact permission@sei.cmu.edu
  for full terms.

  [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
  distribution.  Please see Copyright notice for non-US Government use and distribution.

  This Software includes and/or makes use of Third-Party Software each subject to its own license.

  DM26-0670
-->

# Adding a user to the ACE Gitea server

The ACE Gitea server is a single Ubuntu VM in Azure Government. Only SSH (port 22) is
open. People reach the web UI through an SSH tunnel and use git over SSH.

| | |
|---|---|
| Host | `52.235.181.217` (VM `gitea`, resource group `gitea-rg`, usgovvirginia) |
| Web UI | `http://localhost:3000` through the tunnel |
| Repo | `SEI/ACE` |
| Admin | `djshepard` (sole owner of the `SEI` org, only sudo user on the VM) |

## Rules

- **Published GitHub keys required.** No keys on `https://github.com/<github-user>.keys`
  means no account.
- **One username everywhere:** their SEI email address without `@sei.cmu.edu`
  (`vacoates@sei.cmu.edu` → `vacoates`). The VM login and the Gitea login are the same name.
- **No sudo.** New users go in the `tunnel` group. That lets them log in and forward to
  the Gitea web UI on port 3000, and nothing else.

## Steps

All of this runs from your dev laptop. You don't need Azure or Cloud Shell. In the
commands below, replace:

- `NEWUSER`: their username, for example `wrn`
- `GHUSER`: their GitHub username, for example `vonjackets`
- `EMAIL`: their SEI email address

### 1. Check their GitHub keys

```bash
curl -s https://github.com/GHUSER.keys
```

If this prints nothing, stop. They need to add an SSH key to their GitHub account first.

### 2. Create the VM account

```bash
ssh gitea
```

Then, on the server:

```bash
sudo adduser --disabled-password --gecos "" NEWUSER
sudo usermod -aG tunnel NEWUSER
sudo install -d -m 700 -o NEWUSER -g NEWUSER /home/NEWUSER/.ssh
curl -fsS https://github.com/GHUSER.keys | sudo tee /home/NEWUSER/.ssh/authorized_keys
sudo chown NEWUSER:NEWUSER /home/NEWUSER/.ssh/authorized_keys
sudo chmod 600 /home/NEWUSER/.ssh/authorized_keys
```

### 3. Create the Gitea account

Still on the server:

```bash
sudo -u git gitea --config /etc/gitea/app.ini --work-path /var/lib/gitea \
  admin user create --username NEWUSER --email EMAIL \
  --random-password --random-password-length 24 --must-change-password
```

Copy the `generated random password` it prints. You'll send it to them. It works only
once, because Gitea makes them change it at first sign-in.

Type `exit` to leave the server.

### 4. Add them to the developers team

1. Open the tunnel and web UI: run `ssh gitea`, then browse to `http://localhost:3000`.
2. Go to **SEI → Teams → developers → Add Team Member** and add `NEWUSER`.

The `developers` team has write access to every repo in `SEI`.

### 5. Check their login

From your laptop:

```bash
ssh NEWUSER@52.235.181.217 true
```

This should fail with `Permission denied (publickey)`. That's expected, because you
don't have their key. It shows the account exists and only accepts keys. They confirm
the actual login from their side.

### 6. Send them the blurb

Fill in the blurb below and send it privately (Teams DM or email). Don't put the
one-time password in a shared channel.

## Blurb for the new user

> You now have an account on the ACE Gitea server.
>
> **Username:** `NEWUSER`
> **One-time Gitea password:** `<password>` (you'll be asked to change it at first sign-in, minimum 14 characters)
>
> The server is reachable only over SSH. Your login uses the SSH keys published on your
> GitHub account.
>
> **1. Open the web UI.** Run this and leave the session open:
>
> ```
> ssh -L 3000:localhost:3000 NEWUSER@52.235.181.217
> ```
>
> Then browse to **http://localhost:3000**, sign in, and set your new password.
>
> **2. Add your SSH key to Gitea** so git works. In Gitea, go to **Settings → SSH / GPG
> Keys → Add Key** and paste your public key (the contents of `~/.ssh/<your key>.pub`).
>
> **3. Clone the repo:**
>
> ```
> git clone git@52.235.181.217:SEI/ACE.git
> ```
>
> If ssh picks the wrong key, add `-i ~/.ssh/<your key>` to the `ssh` command. For git,
> run `GIT_SSH_COMMAND="ssh -i ~/.ssh/<your key>" git clone ...`.
>
> **Optional `~/.ssh/config` entries** to shorten the commands to `ssh ace-gitea` and
> `git clone git@ace-gitea-git:SEI/ACE.git`:
>
> ```
> Host ace-gitea
>     HostName 52.235.181.217
>     User NEWUSER
>     LocalForward 3000 localhost:3000
>
> Host ace-gitea-git
>     HostName 52.235.181.217
>     User git
> ```
