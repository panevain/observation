# Deploying to Linode (Akamai Cloud)

The **Deploy** workflow (`.github/workflows/deploy.yml`) puts each new stable release on one Linode that runs `compose.yaml`, with both the app and the region agent. It runs after **Release** has published a `vX.Y.Z` tag. Pre-releases (`vX.Y.Z-dev.N`), `develop` builds, and a tag older than the newest stable release are never deployed on their own.

## How it works

1. **Pick the version.** The `plan` job checks that the tag Release ran for is a stable `vX.Y.Z` and is the newest one in the repository.
2. **Verify the images.** The `deploy` job, in the `production` environment, runs `scripts/verify-image.sh` on both images. Each must have Release's signature and provenance attestation for that tag. The images are then pinned by digest.
3. **Deploy on the host.** The job connects over SSH as `observation-deploy` and sends `deploy X.Y.Z <app digest> <regions digest>`. The deploy key can run nothing but `scripts/deploy-host.sh`, which is installed on the host as `/usr/local/libexec/observation-deploy`. That script:
   - fetches tag `vX.Y.Z`;
   - checks both digests again with that tag's `verify-image.sh` (cosign);
   - runs that tag's `compose.yaml` with `docker compose up`, using the digests and `/etc/observation/observation.env`;
   - waits up to 60 s for `/healthz`.

   If the health check fails, the job fails and prints the containers' logs. Nothing is rolled back on its own.

Settings and secrets (`OBS_CONTACT`, `OBS_REGIONS_TOKEN`, `OBS_ADMIN_TOKEN` and so on) live only in the host's env file. GitHub holds only the deploy key and the host's address. A leaked deploy key can redeploy a release that this repository signed, and nothing more.

## One-time setup

### 1. The Linode

- **Create the instance.** Use Ubuntu 24.04 LTS. Size the disk for the regions you plan to import, since each one is a Geofabrik extract plus its Overpass database.
- **Install the tools:**

  ```sh
  sudo apt-get update && sudo apt-get install -y docker.io docker-compose-v2 git curl
  ```

- **Set up a Cloud Firewall.** Allow inbound TCP 22, 80 and 443.
  - Port 22 has to be open to GitHub-hosted runners, whose addresses change, so allow SSH keys only (`PasswordAuthentication no`).
  - The app itself listens on 127.0.0.1 only.

### 2. The deploy key and the host user

On your own machine:

```sh
ssh-keygen -t ed25519 -N '' -C observation-deploy -f observation-deploy
```

On the Linode, as an admin user:

```sh
git clone https://github.com/panevain/observation.git && cd observation
sudo scripts/deploy-host-setup.sh "ssh-ed25519 AAAA... observation-deploy"   # the contents of observation-deploy.pub
sudoedit /etc/observation/observation.env                                    # set OBS_CONTACT, optionally OBS_ADMIN_TOKEN
```

`deploy-host-setup.sh` does the following:

- installs cosign at the version and checksum in `mise.ci.lock`;
- creates `observation-deploy`, a member of the `docker` group;
- installs the forced command, and writes `authorized_keys` as a root-owned file;
- writes the env file with a generated `OBS_REGIONS_TOKEN`;
- prints the host's public keys for `DEPLOY_KNOWN_HOSTS`.

Run it again after a `git pull` to update the forced command or cosign, or to replace the key.

### 3. TLS in front

The env file publishes the app on `127.0.0.1:8080`. Put [Caddy](https://caddyserver.com/docs/install#debian-ubuntu-raspbian) on the host. Point your domain's DNS at the Linode, then add this to `/etc/caddy/Caddyfile`:

```caddyfile
observation.example.com {
	reverse_proxy 127.0.0.1:8080
}
```

Caddy gets the certificate and sets `X-Forwarded-For`. The app sees Caddy's connections as coming from Docker's bridge gateway, which is why the env file sets `OBS_TRUSTED_PROXIES=172.16.0.0/12`.

### 4. GitHub

1. **Settings → Environments → New environment** `production`. Under **Deployment branches and tags**, choose **Selected branches** and add `develop`. Both `workflow_run` and manual runs of Deploy run on `develop`. Add required reviewers if a person should approve each deploy.
2. Add the environment's secret and variables:

   ```sh
   gh secret set DEPLOY_SSH_KEY --env production < observation-deploy
   gh variable set DEPLOY_HOST --env production --body observation.example.com
   gh variable set DEPLOY_KNOWN_HOSTS --env production --body "observation.example.com ssh-ed25519 AAAA..."
   gh variable set DEPLOY_URL --env production --body https://observation.example.com   # optional, shown on the deployment
   ```

   - `DEPLOY_KNOWN_HOSTS` is the line `deploy-host-setup.sh` printed, with `<DEPLOY_HOST>` replaced by `DEPLOY_HOST`'s value. You can check it from your machine with `ssh-keyscan -t ed25519 <host>`.
   - Delete the private key file once it is stored.
   - `DEPLOY_USER` is optional and defaults to `observation-deploy`.
3. **Actions → Deploy → Run workflow** from `develop`, with the version empty, to deploy the newest release now.
4. Turn on automatic deploys:

   ```sh
   gh variable set DEPLOY_PRODUCTION --body true
   ```

## Rolling back and redeploying

- **Roll back or redeploy:** go to **Actions → Deploy → Run workflow** and enter the version (`X.Y.Z`). Any stable release can be deployed.
- **After changing the env file:** redeploy the current version by running the workflow with the version empty, or deploy on the host directly:

  ```sh
  sudo -u observation-deploy /usr/local/libexec/observation-deploy deploy X.Y.Z
  ```

- **Check what is running:** `/var/lib/observation-deploy/deployed` records the deployed version and digests.

The `observation_cache` and `observation_regions` volumes carry over from one deploy to the next. Images of earlier releases are pruned once a deploy succeeds.
