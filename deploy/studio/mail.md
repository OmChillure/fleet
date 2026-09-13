# Email — agentfleet.studio

No mail daemon on the VPS. Receive is Cloudflare Email Routing. Send is
Resend over :587/:465 (the VPS cannot talk to the public internet on :25).

| Direction | Who | Address |
|---|---|---|
| Inbound | Cloudflare → your inbox | `hello@`, `support@`, `admin@` @agentfleet.studio |
| Outbound | App → Resend | `noreply@mail.agentfleet.studio` |
| Replies | people write to | `hello@agentfleet.studio` |

Sending from the `mail.` subdomain keeps Resend’s SPF/DKIM off the apex, so
Cloudflare can lock the MX/SPF it needs for Routing.

## 1. Receive (Cloudflare)

In the Cloudflare dashboard for **agentfleet.studio**:

1. **Email** → **Email Routing** → **Get started** → **Add records and enable**
2. **Destination addresses** → add the inbox you actually read (Gmail etc.) and
   verify the link they send
3. **Routing rules** → create:

   | Custom address | Destination |
   |---|---|
   | `hello` | your inbox |
   | `support` | your inbox |
   | `admin` | your inbox |

4. Optional: enable **Catch-all** → same inbox

Cloudflare adds the MX hosts (`*.mx.cloudflare.net`) and an apex SPF
`include:_spf.mx.cloudflare.net` itself. Leave those locked.

## 2. Send (Resend)

1. Create an account at https://resend.com
2. **Domains** → **Add Domain** → `mail.agentfleet.studio`
3. Copy every record Resend shows into Cloudflare DNS. **Grey-cloud**
   (DNS only) every CNAME. Typical set:

   | Type | Name | Notes |
   |---|---|---|
   | MX | `send.mail` | Resend bounce/feedback host |
   | TXT | `send.mail` | Resend SPF |
   | CNAME | `*.mail` DKIM hosts | three `._domainkey` names |
   | CNAME | `links.mail` | optional click tracking |

   Do **not** edit the apex MX/SPF that Email Routing owns.
4. Wait until the domain is **Verified**
5. **API Keys** → create a sending key (`re_…`)
6. On the VPS, put it in `/home/agent-fleet/stack/.env` (see [env.example](env.example)):

   ```bash
   ssh -i ~/.ssh/agent-fleet agent-fleet@82.39.213.33
   nano /home/agent-fleet/stack/.env
   ```

   ```
   SMTP_HOST=smtp.resend.com
   SMTP_PORT=465
   SMTP_USERNAME=resend
   SMTP_PASSWORD=re_xxxxxxxx
   SMTP_FROM=Agent Fleet <noreply@mail.agentfleet.studio>
   SMTP_REPLY_TO=hello@agentfleet.studio
   RESEND_API_KEY=re_xxxxxxxx
   ```

7. Prove send (from this PC):

   ```bash
   ./scripts/send-test-mail.sh you@example.com
   ```

Postmark, SES, or Mailgun work the same way: change `SMTP_*` and verify *their*
DNS on `mail.agentfleet.studio` instead.

## 3. DMARC (after a test message lands)

Apex TXT `_dmarc`:

```
v=DMARC1; p=none; rua=mailto:admin@agentfleet.studio
```

Tighten `p=` later. Reports go to `admin@`, which Routing already forwards.
