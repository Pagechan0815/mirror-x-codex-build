#!/usr/bin/env bash
set -euo pipefail

release_id="${1:?release id is required}"
stage_dir="/var/www/mirror-x-mobile/.stage-${release_id}"
web_dir="/var/www/mirror-x-mobile"
nginx_conf="/etc/nginx/conf.d/relay.conf"
backup_dir="/var/backups/mirror-x-mobile/${release_id}"

files=(index.html app.js relay.js crypto.js style.css manifest.json icon.svg)

mkdir -p "${backup_dir}"
for file in "${files[@]}"; do
  test -s "${stage_dir}/${file}"
  if test -f "${web_dir}/${file}"; then
    cp -a "${web_dir}/${file}" "${backup_dir}/${file}"
  fi
done
cp -a "${nginx_conf}" "${backup_dir}/relay.conf"

python3 - "${nginx_conf}" <<'PY'
from pathlib import Path
import re
import sys

path = Path(sys.argv[1])
text = path.read_text(encoding="utf-8")

for asset in ("mobile", "app.js", "relay.js", "crypto.js", "style.css", "manifest.json", "icon.svg"):
    pattern = re.compile(
        rf"\n\s*location = /relay/{re.escape(asset)} \{{.*?\n\s*\}}\n",
        re.DOTALL,
    )
    text = pattern.sub("\n", text)

locations = r'''
    location = /relay/mobile {
        alias /var/www/mirror-x-mobile/index.html;
        default_type text/html;
        add_header Cache-Control "no-store, no-cache, must-revalidate, max-age=0" always;
    }

    location = /relay/app.js {
        alias /var/www/mirror-x-mobile/app.js;
        default_type application/javascript;
        add_header Cache-Control "no-store, no-cache, must-revalidate, max-age=0" always;
    }

    location = /relay/relay.js {
        alias /var/www/mirror-x-mobile/relay.js;
        default_type application/javascript;
        add_header Cache-Control "no-store, no-cache, must-revalidate, max-age=0" always;
    }

    location = /relay/crypto.js {
        alias /var/www/mirror-x-mobile/crypto.js;
        default_type application/javascript;
        add_header Cache-Control "no-store, no-cache, must-revalidate, max-age=0" always;
    }

    location = /relay/style.css {
        alias /var/www/mirror-x-mobile/style.css;
        default_type text/css;
        add_header Cache-Control "no-store, no-cache, must-revalidate, max-age=0" always;
    }

    location = /relay/manifest.json {
        alias /var/www/mirror-x-mobile/manifest.json;
        default_type application/manifest+json;
        add_header Cache-Control "no-store, no-cache, must-revalidate, max-age=0" always;
    }

    location = /relay/icon.svg {
        alias /var/www/mirror-x-mobile/icon.svg;
        default_type image/svg+xml;
        add_header Cache-Control "public, max-age=86400" always;
    }

'''
marker = "    location /relay {"
if text.count(marker) != 1:
    raise SystemExit("expected one relay proxy location")
text = text.replace(marker, locations + marker, 1)
path.write_text(text, encoding="utf-8", newline="\n")
PY

for file in "${files[@]}"; do
  mv -f "${stage_dir}/${file}" "${web_dir}/${file}"
done

if ! nginx -t; then
  cp -a "${backup_dir}/relay.conf" "${nginx_conf}"
  for file in "${files[@]}"; do
    if test -f "${backup_dir}/${file}"; then
      cp -a "${backup_dir}/${file}" "${web_dir}/${file}"
    else
      rm -f "${web_dir:?}/${file}"
    fi
  done
  nginx -t
  exit 1
fi

systemctl reload nginx
rm -rf "${stage_dir}"

sha256sum "${files[@]/#/${web_dir}/}"
systemctl is-active nginx
systemctl is-active mirror-x-relay
