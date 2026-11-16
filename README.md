# Landing page (PHP) for the two projects

Single page for the judges: links to both apps and both repositories, plus a
keep-alive request to the Render instance of SPHEREx Blink.

- `index.php`: the page. Projects and links are the `$projects` array at the top.
- `ping.php`: sends one GET to `https://spherex-4e2z.onrender.com/api/v1/health`
  and returns JSON. Takes no input. At most one real request every 240 s, however
  many visitors or cron runs call it.

## Deploy on Hostinger (shared hosting, PHP)

Upload `index.php` and `ping.php` to `public_html/` (hPanel File Manager, FTP, or
the Git integration pointed at this branch, `landing-php`). No build step.

## Keep Render awake without visitors

The page pings every 5 minutes only while someone has it open. For round-the-clock
keep-alive add a cron job in hPanel → Advanced → Cron Jobs, every 5 minutes:

```
*/5 * * * * curl -fsS -m 90 https://YOUR-DOMAIN/ping.php > /dev/null
```

Render's free plan sleeps after about 15 minutes without traffic, so 5 minutes is
enough margin.
