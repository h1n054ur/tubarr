---
title: Where to run it
description: Why a home connection works best, and what to do when Tubarr has to run on a VPS or a cloud host.
---

## The short answer

Run Tubarr on a home connection if you can. YouTube treats traffic from cloud and VPS address ranges with much more suspicion than traffic from a normal household line. On those ranges yt-dlp is often met with "Sign in to confirm you're not a bot" within the first few lookups. On a small test VPS, yt-dlp reached YouTube and got that check straight away. A home connection did not.

Tubarr has no switch that changes this. What you can choose is where its YouTube traffic leaves from. If Tubarr has to live on a VPS, there are two ways to do that, described below.

Please only use a network you are allowed to use, and keep in mind that [YouTube's terms](../../about/terms/) still apply wherever the traffic leaves from.

## Running on a VPS

### Option A: set a proxy

Tubarr can send its YouTube traffic through a proxy that you run somewhere you are allowed to. A common shape is a small SOCKS proxy at home, reached from the VPS over WireGuard or Tailscale. Put the proxy in the `[network]` section of `tubarr.toml` (see [`[network]`](../../setup/configuration/#network) in the configuration reference):

```toml
[network]
proxy = "socks5h://10.8.0.2:1080"
```

`proxy` takes an `http`, `https`, `socks5` or `socks5h` URL. With `socks5h` the proxy looks up host names itself, which is usually what you want.

If the proxy needs a user name and password, keep them out of the config file. Put the whole URL in a file instead and point `proxy_file` at it:

```toml
[network]
proxy_file = "/run/secrets/tubarr_proxy"
```

The file holds one line, for example `socks5h://user:password@10.8.0.2:1080`. Setting both `proxy` and `proxy_file` is an error, and Tubarr will not start. Credentials are never written to the log or shown on the Health page.

What the proxy covers, and what it does not:

- It covers Tubarr's own YouTube traffic: yt-dlp, feed reads, channel listings, thumbnails and the play relay.
- It does not cover Tubarr's calls to Sonarr, Radarr or Jellyfin, the notify webhook, OIDC sign-in, or the downloads of yt-dlp and deno from GitHub. Those go out the normal way. yt-dlp also reaches loopback and the PO token server (`[fallback] po_token_url`) directly, not through the proxy.

With `[network] proxy` set, yt-dlp hands the proxy to the PO token helper with each token request, so the helper's own requests to YouTube go through the same proxy. The helper's log names the proxy with the password masked but the user name shown.

The play relay carries the video itself, so a play streams through the proxy. Check that its link has the bandwidth for the quality you watch.

### Option B: run Tubarr behind a VPN container

The other way is to give Tubarr the network of a VPN container. With Docker's `network_mode: service:<name>` the two containers share one network stack, so everything Tubarr sends leaves through the tunnel. This example uses a WireGuard client container. Replace the placeholders with the values your own WireGuard provider or server gave you.

```yaml
services:
  wireguard:
    image: lscr.io/linuxserver/wireguard:latest
    container_name: wireguard
    cap_add:
      - NET_ADMIN
    sysctls:
      - net.ipv4.conf.all.src_valid_mark=1
    environment:
      PUID: "1000"
      PGID: "1000"
    volumes:
      # Holds wg0.conf: [Interface] PrivateKey = <your private key>, Address = <tunnel address>,
      # [Peer] PublicKey = <server public key>, Endpoint = <server:port>, AllowedIPs = 0.0.0.0/0
      - ./wireguard:/config
    ports:
      # Tubarr's web UI and /play are reached through this container, so publish them here.
      - "8480:8480"
    restart: unless-stopped

  tubarr:
    image: ghcr.io/h1n054ur/tubarr:0.1
    network_mode: "service:wireguard"
    depends_on:
      - wireguard
    mem_limit: 1g
    volumes:
      - ./config:/config
      - /srv/media/downloads:/downloads
    restart: unless-stopped
```

Things to know:

- A container that uses `network_mode: service:...` cannot publish ports of its own. Publish Tubarr's port (8480 by default) on the VPN container, as above.
- Other containers reach Tubarr at the VPN container's name, so Sonarr, Radarr and Jellyfin use `http://wireguard:8480` if they share its Docker network. `public_url` has to be an address they can reach.
- Everything Tubarr sends goes through the tunnel, including its calls to Sonarr, Radarr and Jellyfin. Make sure the tunnel config still lets those through, for example by leaving your local ranges out of `AllowedIPs` or adding a route for them.
- Use the image name and tag from your own [install](../../getting-started/install/), and the memory limit from [Sizing](../sizing/).

If you can pick between the two, the proxy is the smaller change. It leaves your containers as they are and covers only the YouTube traffic.

## What Tubarr does when YouTube pushes back

This is deliberate. When yt-dlp gets a 429 or a bot check, Tubarr stops asking for a while rather than sending more lookups that would likely be refused too.

- **Lookups pause.** Every lookup is held for `[rate_limit] throttle_pause` (15 minutes by default) and the token bucket is emptied, so nothing bursts when the pause ends. A fresh throttle during a pause extends it if it would end later.
- **Plays are refused with 503.** A play that hits the throttle gets a `503` with a `Retry-After` header. The value is the time left on the pause, and never less than 30 seconds. Jellyfin shows a playback error and a later try can work.
- **Feed reads wait.** The feed loop skips a channel while lookups are paused and schedules it again for when the pause ends. The daily listings, playlists and back-episode searches hold off the same way.
- **Newest uploads still come in.** If a channel's feed has failed for longer than one feed interval, Tubarr lists that channel's newest uploads (`[watcher] fallback_listing_size`, 15 by default) at about the feed's pace, at most `fallback_per_hour` (30 by default) across all channels. Those listings are lookups too, so they wait while lookups are paused.
- **The canary keeps watch.** If you set `[canary] video_id`, Tubarr looks that video up about every hour. The canary does not run while lookups are paused or busy. After three failed canary lookups in a row it checks for a new yt-dlp and sends a notice if cards are set up. A later success sends a "works again" notice.

The pause ends on its own clock. The canary does not decide when lookups resume, it tells you whether they work once they do.

## How to tell it is working

**The Health page.** Under YouTube lookups, the rate limiter row says either when the next token is ready or "Paused for ... after YouTube pushed back". While paused, the overall status is degraded with the reason "lookups are paused after YouTube pushed back". The Canary section lists recent runs and how many ticks were skipped. The number of canary failures in a row is a tile at the top of the page.

**The log.** These lines are at `info` and `warn`:

| Line | Meaning |
|---|---|
| `YouTube is throttling, pausing lookups` | a lookup got a 429 or a bot check and the pause started |
| `play refused while YouTube throttles` | a play got the 503 above |
| `canary lookup failed` | the hourly lookup failed, with the error |
| `canary keeps failing, forcing a yt-dlp update` | three failures in a row, at most once a day |
| `canary works again` | the lookup passed after a streak of failures |

A good sign after a change of route is the canary passing and the pause not coming back. If it still gets a bot check at once, the new route is treated the same way as the old one, and [Troubleshooting](../troubleshooting/#youtube-lookups-fail) lists what else to try.
