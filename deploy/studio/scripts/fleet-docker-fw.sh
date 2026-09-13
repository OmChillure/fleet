#!/bin/bash
# Restrict Docker-published 80/443 inbound to Cloudflare.
# Do not match container *egress* on dport 80/443 — that is API→Slack/GitHub/Grok.
#
# Live copy: /usr/local/sbin/fleet-docker-fw (fleet-docker-fw.service).
# If you edit this file, scp it to the VPS and run it; do not only change the live copy.
set -euo pipefail
iptables -N DOCKER-USER 2>/dev/null || true
iptables -F DOCKER-USER
iptables -A DOCKER-USER -m conntrack --ctstate RELATED,ESTABLISHED -j RETURN
# Container-originated traffic (OAuth token exchange, SMTP, LLM APIs).
iptables -A DOCKER-USER -s 10.0.0.0/8 -j RETURN
iptables -A DOCKER-USER -s 172.16.0.0/12 -j RETURN
iptables -A DOCKER-USER -s 192.168.0.0/16 -j RETURN
while read -r cidr; do
  [[ -n "$cidr" ]] || continue
  iptables -A DOCKER-USER -s "$cidr" -p tcp --dport 80 -j RETURN
  iptables -A DOCKER-USER -s "$cidr" -p tcp --dport 443 -j RETURN
done <<'V4'
173.245.48.0/20
103.21.244.0/22
103.22.200.0/22
103.31.4.0/22
141.101.64.0/18
108.162.192.0/18
190.93.240.0/20
188.114.96.0/20
197.234.240.0/22
198.41.128.0/17
162.158.0.0/15
104.16.0.0/13
104.24.0.0/14
172.64.0.0/13
131.0.72.0/22
V4
iptables -A DOCKER-USER -p tcp --dport 80 -j DROP
iptables -A DOCKER-USER -p tcp --dport 443 -j DROP

ip6tables -N DOCKER-USER 2>/dev/null || true
ip6tables -F DOCKER-USER
ip6tables -A DOCKER-USER -m conntrack --ctstate RELATED,ESTABLISHED -j RETURN
ip6tables -A DOCKER-USER -s fc00::/7 -j RETURN
ip6tables -A DOCKER-USER -s fe80::/10 -j RETURN
while read -r cidr; do
  [[ -n "$cidr" ]] || continue
  ip6tables -A DOCKER-USER -s "$cidr" -p tcp --dport 80 -j RETURN
  ip6tables -A DOCKER-USER -s "$cidr" -p tcp --dport 443 -j RETURN
done <<'V6'
2400:cb00::/32
2606:4700::/32
2803:f800::/32
2405:b500::/32
2405:8100::/32
2a06:98c0::/29
2c0f:f248::/32
V6
ip6tables -A DOCKER-USER -p tcp --dport 80 -j DROP
ip6tables -A DOCKER-USER -p tcp --dport 443 -j DROP
