# Hosting the dashboard and the facilitator on a VM

Written 9 Nov 2026 for Steven's VM. Section 1 is what was actually run on it that day, over SSH, and the dashboard has been live at https://nearx402.svmtech.xyz/ since. Section 2 has not been run anywhere yet: its commands were checked against `crates/nearx402/src/bin/facilitator.rs`, not against a machine. Treat its first run as a rehearsal and note what differs.

Two things can be hosted. They are independent, and the dashboard alone is enough for judges.

| Part | What it is | Needs a key? | Needs to be public? |
| --- | --- | --- | --- |
| Dashboard | `dashboard/`, three static files. Reads the policy contract through public RPC from the visitor's browser | No | Yes, that is the point |
| Facilitator | `facilitator` binary. Answers `/verify` and `/settle` for a vendor; reads the chain, holds no NEAR key | No NEAR key. One API key per vendor, made by you | Only to the vendor that uses it |

The vendor itself is not packaged as a service. The demo's vendor runs inside `demo-zk --serve` together with a facilitator and the dashboard, bound to `127.0.0.1`, and that mode has only been run against a local sandbox. Hosting a live vendor is a separate piece of work.

## 1. Dashboard

Any static web server works. The page reads `config.json` next to it, so the three files must stay together: `index.html`, `config.json`, `transactions.json`.

The docs pages under `/docs/` are built, not written: `python3 scripts/build-docs.py` (no dependencies) renders `README.md` and the public files in `docs/` to `dashboard/docs/*.html`, in the dashboard's own style, which it reads from `dashboard/index.html` at build time. `dashboard/docs/` is git-ignored; the Pages workflow runs the script before it uploads, and for the VM you run it before copying. Add `dashboard/docs/*.html` to the `scp` line below and install them into `/srv/nearx402-dashboard/docs/`.

Since 9 Nov the page loads its two typefaces, Figtree and JetBrains Mono, from Google Fonts, so a visitor's browser makes one request to `fonts.googleapis.com` and a few to `fonts.gstatic.com` besides the RPC calls. If that is unwanted, drop the three `<link>` lines in the head: the system font fallbacks are in every `font-family`.

`config.json` holds only public values: the contract ids, the expected code hash, the RPC URL, the explorer base and the repository URL. If the contract is ever redeployed, change `expected_code_hash` there, or the page's first check turns red, which is what it is for.

The VM already ran nginx for another site, bound as `default_server` on port 80, so the dashboard got its own server block for its own name and nothing else was touched. Done on 9 Nov with the A record `nearx402.svmtech.xyz` pointing at the VM and ports 80 and 443 open in the cloud firewall:

```bash
scp dashboard/index.html dashboard/config.json dashboard/transactions.json ubuntu@VM:/tmp/nearx402-dash/
sudo mkdir -p /srv/nearx402-dashboard
sudo install -m 0644 -o root -g root /tmp/nearx402-dash/* /srv/nearx402-dashboard/
```

`/etc/nginx/sites-available/nearx402`, before certbot:

```
server {
    listen 80;
    listen [::]:80;
    server_name nearx402.svmtech.xyz;

    root /srv/nearx402-dashboard;
    index index.html;

    add_header Cache-Control "no-store" always;
    add_header X-Content-Type-Options "nosniff" always;
    add_header Referrer-Policy "no-referrer" always;

    location / {
        try_files $uri $uri/ =404;
    }
}
```

```bash
sudo ln -s /etc/nginx/sites-available/nearx402 /etc/nginx/sites-enabled/nearx402
sudo nginx -t && sudo systemctl reload nginx
sudo apt-get install -y certbot python3-certbot-nginx
sudo certbot --nginx -d nearx402.svmtech.xyz --non-interactive --agree-tos --register-unsafely-without-email --redirect
```

certbot added the `listen 443 ssl` lines and the certificate paths to that file, turned the port 80 block into a redirect to HTTPS, and installed `certbot.timer` for renewal. The certificate was issued 9 Nov and expires 7 Feb 2027; the timer renews it before then without anyone's help. The registration has no email address, so nobody gets an expiry warning if renewal ever fails: `sudo certbot renew --dry-run` is the check.

The `no-store` header matters: the page refreshes every ten seconds and must not get a cached `transactions.json` or `config.json` after you change them. The visitor's browser reads the chain itself, through `rpc` in `config.json` and, when that one rate-limits the visitor's IP with a 429 or fails, through the URLs in `rpc_fallback`, in order; the VM makes no RPC calls for the dashboard. Since 9 Nov the order is dRPC (`near-testnet.drpc.org`, free tier published as about 2,100 CU/s, answered 30 back-to-back reads with 200), then FastNear (`test.rpc.fastnear.com`, limit not published, rate-limited two visitors on 9 Nov), then Intear (`testnet-rpc.intea.rs`, slows unkeyed requests to one per second). `rpc.testnet.near.org` is gone: it answers 429 with a deprecation notice to every request, so it must not appear in any config. To update the page, copy the three files again with the same `scp` and `install` lines; nginx needs no reload for static files.

The same folder is also what `.github/workflows/pages.yml` publishes to GitHub Pages once the repository is public and Pages is set to "GitHub Actions". Either host works; both can run at once.

What was checked after the deploy, in a browser, and what to check again after any change:

1. The five rows under "Verify the deployment" are green except "Contract keys", which is yellow on purpose (the contract is not locked).
2. The audit log shows seven entries with transaction links, the newest first, and "Where the money went" draws two green and two red ribbons from them.
3. Narrow the window to phone width: no horizontal scroll.
4. Open the browser console: no errors.
5. `http://` redirects to `https://` with a 301, and the certificate is for the right name.

## 2. Facilitator

### Build

On a machine with Rust stable (the VM, or build elsewhere and copy the binary; the target must match the VM's libc):

```bash
cargo build --release -p nearx402 --bin facilitator
sudo install -m 0755 target/release/facilitator /usr/local/bin/nearx402-facilitator
```

### Configuration

Every option is a flag or an environment variable. The values below are for testnet and the current build.

| Variable | Value | Why |
| --- | --- | --- |
| `NEAR_RPC_URL` | `https://test.rpc.fastnear.com` | The public `rpc.testnet.near.org` returned 429 during the failed take on 8 Nov |
| `X402_NETWORK` | `near-testnet` | Goes into `/supported` and must match what the agent sends |
| `TOKEN_ID` | `nearx402-musdc.testnet` | The only token whose `ft_transfer` event the facilitator accepts |
| `POLICY_CODE_HASH` | `6BA4L1moNbXF8iUuZTzk4ag93cRwtE5mmqWBu83mQsTx` | A receipt from a contract with any other code is refused. Change it after a redeploy |
| `HOST` | `127.0.0.1` | Keep it local and put Caddy in front for TLS. The receipt secret travels in the request, so never serve this over plain HTTP across a network |
| `PORT` | `8403` | |
| `FACILITATOR_API_KEYS` | `<random>:nearx402-data.testnet` | One `KEY:vendor` entry per vendor, space-separated. Make the key with `openssl rand -hex 32`. The vendor sends it as `Authorization: Bearer <KEY>` and may only settle receipts addressed to its own account |
| `REDEEMED_FILE` | `/var/lib/nearx402/redeemed.tsv` | Append-only record of settled receipts. Must survive restarts, or a receipt could be served twice. One file, one process: do not run two facilitators for the same vendor |
| `MAX_PAYMENT_AGE_SECS` | `600` | Must stay above the 300-second nonce lifetime |

### systemd

`/etc/nearx402/facilitator.env`, owned by root, mode `0600`, because it holds the vendors' API keys:

```
NEAR_RPC_URL=https://test.rpc.fastnear.com
X402_NETWORK=near-testnet
TOKEN_ID=nearx402-musdc.testnet
POLICY_CODE_HASH=6BA4L1moNbXF8iUuZTzk4ag93cRwtE5mmqWBu83mQsTx
HOST=127.0.0.1
PORT=8403
FACILITATOR_API_KEYS=REPLACE_WITH_openssl_rand_hex_32:nearx402-data.testnet
REDEEMED_FILE=/var/lib/nearx402/redeemed.tsv
MAX_PAYMENT_AGE_SECS=600
```

`/etc/systemd/system/nearx402-facilitator.service`:

```
[Unit]
Description=nearx402 x402 facilitator (reads NEAR, holds no key)
After=network-online.target
Wants=network-online.target

[Service]
User=nearx402
Group=nearx402
EnvironmentFile=/etc/nearx402/facilitator.env
ExecStart=/usr/local/bin/nearx402-facilitator
Restart=on-failure
RestartSec=3
StateDirectory=nearx402
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
PrivateTmp=true
ReadWritePaths=/var/lib/nearx402

[Install]
WantedBy=multi-user.target
```

```bash
sudo useradd --system --home /var/lib/nearx402 --shell /usr/sbin/nologin nearx402
sudo mkdir -p /etc/nearx402 && sudo chmod 0700 /etc/nearx402
sudo systemctl daemon-reload
sudo systemctl enable --now nearx402-facilitator
sudo systemctl status nearx402-facilitator
```

### TLS in front

A second nginx server block, for its own name, then certbot for that name as in section 1:

```
server {
    listen 80;
    listen [::]:80;
    server_name facilitator.example.com;
    location / {
        proxy_pass http://127.0.0.1:8403;
        proxy_set_header Host $host;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
    }
}
```

The vendor is then configured with `https://facilitator.example.com` and its API key.

### Check

```bash
curl -s https://facilitator.example.com/supported
```

should list the `near-policy` scheme on `near-testnet`. A `/verify` or `/settle` without a Bearer key must return 401, and with a key for another vendor 403. On start the service log (`journalctl -u nearx402-facilitator`) prints the network, the RPC, the token, the trusted code hash and a prefix of each API key, never the whole key.

## 3. What this does not cover

- Backups of `redeemed.tsv`. Losing it allows one replay per already-served receipt whose claim window is still open (ten minutes); after that the age check refuses them anyway.
- Rate limiting at nginx. The facilitator makes RPC calls per request; a hostile vendor with a key could drive RPC load. The keys are per vendor, so revoke the key.
- Monitoring. Nothing alerts if the service stops; `Restart=on-failure` is the only recovery.
- Mainnet. Different RPC, token, code hash and `X402_NETWORK`, and the open items in the README's limits before any real funds.
