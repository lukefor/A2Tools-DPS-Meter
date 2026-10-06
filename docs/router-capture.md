# Capturing on an OpenWrt router

The meter normally reads the game's traffic on the PC the game runs on, through Npcap. It can read it on your router instead: the meter connects to the router over SSH, runs `tcpdump` there, and reads the packets it streams back. The PC then needs no Npcap at all.

The meter still runs on the PC with the game, because it draws over the game and watches for its window. Only packet capture moves to the router.

## What you need

- An **OpenWrt** router that your PC's traffic goes through (your gateway). Another Linux router with `tcpdump` and SSH works the same.
- **Windows' OpenSSH client.** Windows 10 and 11 include it; if `ssh` is not a command in PowerShell, add **OpenSSH Client** under Settings > System > Optional features.
- About 15 minutes for the setup below, done once.

## Setup

### 1. Give the PC an SSH key

The meter signs in to the router with a key, since it can't type a password. In PowerShell:

```powershell
ssh-keygen -t ed25519
```

Press Enter at each prompt to accept the default file and **no passphrase**. A key with a passphrase also works, but only if the Windows `ssh-agent` service is running and holds it.

### 2. Let the router accept the key

Either paste the contents of `%USERPROFILE%\.ssh\id_ed25519.pub` into LuCI under **System > Administration > SSH-Keys**, or run this in PowerShell (it asks for the router's password once):

```powershell
type $env:USERPROFILE\.ssh\id_ed25519.pub | ssh root@192.168.1.1 "cat >> /etc/dropbear/authorized_keys"
```

### 3. Install tcpdump on the router

`tcpdump-mini` is enough and is the smallest. On OpenWrt 24.10 and earlier:

```sh
opkg update && opkg install tcpdump-mini
```

On OpenWrt 25 and later, which use `apk`:

```sh
apk update && apk add tcpdump-mini
```

### 4. Check it from the PC

This should print five packets and return without asking for a password:

```powershell
ssh root@192.168.1.1 "tcpdump -i br-lan -c 5 tcp"
```

### 5. Turn off flow offloading

OpenWrt's **software and hardware flow offloading** forward an established connection in a fast path that `tcpdump` cannot see. The meter would see the game connect and then nothing at all. Turn both off under **Network > Firewall > Routing/NAT Offloading**, or:

```sh
uci set firewall.@defaults[0].flow_offloading='0'
uci set firewall.@defaults[0].flow_offloading_hw='0'
uci commit firewall && service firewall restart
```

On a fast internet connection this can lower the router's top speed, since the CPU now forwards every packet. Game traffic itself is small.

### 6. Tell the meter

In the meter's **Settings > Connection > Capture on router**, enter the same address you used with `ssh`, e.g. `root@192.168.1.1`, then press Enter. The meter switches over at once. **Detected device/port** shows what went wrong if it cannot connect, and shows the router once the game is found.

To go back to capturing on the PC, clear the field.

## What the router sends

Only the game's connection. The filter runs on the router itself:

```
tcp and host <your PC> and port 13328
```

13328 is the game server's port. Nothing else your PC does, and nothing from any other device on your network, ever leaves the router, so the meter never sees it. The SSH connection is excluded too.

The meter reconnects on its own if the router restarts or the connection drops.

If your region's game server uses a different port, the meter will never detect the game. Set `capture.routerFilter` (below) to `tcp and port <that port>`.

## Advanced settings

These have no field in Settings. Close the meter, then edit `settings.json` in `%APPDATA%\com.a2tools.dps-meter\`:

| Key | Default | What it does |
| --- | --- | --- |
| `capture.router` | (empty) | The `ssh` destination. Anything `ssh` accepts works, including `ssh://root@192.168.1.1:2222` and a Host name from `%USERPROFILE%\.ssh\config`. |
| `capture.routerInterface` | `br-lan` | The router interface to capture on. Use the interface your PC's traffic reaches the router on. |
| `capture.routerFilter` | `tcp and host <your PC> and port 13328` | A `tcpdump` filter used instead of the default. The SSH connection is always left out. |

## Troubleshooting

**"The router refused this PC's SSH key"**: the key from step 1 is not in the router's `authorized_keys` (step 2), or the meter runs as a different Windows user than the one who made the key.

**"tcpdump is not installed on the router"**: do step 3. Packages are removed by a firmware upgrade unless you keep them, so check again after one.

**Connected, but the game is never detected**: check step 5. Also check the PC reaches the router on `br-lan`. If the PC is plugged in to a separate access point, capture on the router that is the gateway.

**Why not run the whole meter on the router?** The meter is a desktop app that draws over the game, and a router has neither the memory nor a screen for it. Parsing on the PC also keeps the router's work to forwarding packets.
