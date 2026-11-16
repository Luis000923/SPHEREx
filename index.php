<?php
declare(strict_types=1);

// Single-page hub for the two NASA Space Apps 2026 projects of the team.
// Every link lives in this array so it is edited in one place.
$projects = [
    [
        'name' => 'SPHEREx Blink',
        'author' => 'Luis',
        'summary' => 'Enter RA/Dec and watch a time-lapse of SPHEREx images of that patch of sky to spot moving objects. Rust backend, React frontend, live archive data.',
        'app' => 'https://spherex-4e2z.onrender.com/',
        'repo' => 'https://github.com/Luis000923/SPHEREx/',
        'status_id' => 'status-spherex',
    ],
    [
        'name' => 'Planeta X',
        'author' => 'Charlie',
        'summary' => 'Compare two epochs of the same field with a slider or blink, click what moved, draw its A to B path and export your findings. Interface in Spanish and English.',
        'app' => 'https://app.planeta-x.earth',
        'repo' => 'https://github.com/charlie-002-XD/Planeta-X',
        'status_id' => null,
    ],
];
?>
<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Planet X and SPHEREx · Team AirmanPrep</title>
<meta name="description" content="Two web tools for the NASA Space Apps 2026 challenge Planet X and SPHEREx: SPHEREx Blink and Planeta X.">
<style>
  :root {
    --bg: #14150f;
    --card: #1d1f17;
    --border: #34372b;
    --fg: #f1efe4;
    --dim: #c2c0b3;
    --accent: #c6f432;
    --accent-fg: #14150f;
    --ok: #7ddc7d;
    --warn: #f0c24b;
    color-scheme: dark light;
  }
  @media (prefers-color-scheme: light) {
    :root {
      --bg: #f6f3e8;
      --card: #ffffff;
      --border: #d8d3bf;
      --fg: #1c1d15;
      --dim: #504f43;
      --accent: #b6e11f;
      --accent-fg: #14150f;
      --ok: #2f7a2f;
      --warn: #8a6100;
    }
  }
  * { box-sizing: border-box; }
  body {
    margin: 0;
    background: var(--bg);
    color: var(--fg);
    font: 16px/1.55 system-ui, -apple-system, "Segoe UI", Roboto, sans-serif;
  }
  main { max-width: 56rem; margin: 0 auto; padding: 2.5rem 1rem 3rem; }
  h1 { font-size: clamp(1.6rem, 5vw, 2.4rem); line-height: 1.15; margin: 0 0 .5rem; }
  .team { margin: 0 0 .75rem; color: var(--dim); font-size: .9375rem; }
  .team strong { color: var(--fg); }
  .team a { color: var(--fg); font-weight: 600; }
  .lead { color: var(--dim); max-width: 42rem; margin: 0 0 2rem; }
  .grid { display: grid; gap: 1rem; grid-template-columns: repeat(auto-fit, minmax(17rem, 1fr)); }
  .card {
    display: flex; flex-direction: column; gap: .75rem;
    background: var(--card); border: 1px solid var(--border);
    border-radius: 14px; padding: 1.25rem;
  }
  .card h2 { margin: 0; font-size: 1.3rem; }
  .by { color: var(--dim); font-size: .875rem; margin: 0; }
  .card p { margin: 0; color: var(--dim); }
  .actions { display: flex; flex-wrap: wrap; gap: .5rem; margin-top: auto; }
  .btn {
    display: inline-flex; align-items: center; justify-content: center;
    min-height: 44px; padding: 0 1.1rem; border-radius: 10px;
    border: 1px solid var(--border); color: var(--fg);
    text-decoration: none; font-weight: 600; font-size: .9375rem;
  }
  .btn:hover { border-color: var(--fg); }
  .btn-primary { background: var(--accent); color: var(--accent-fg); border-color: var(--accent); }
  .btn:focus-visible { outline: 3px solid var(--fg); outline-offset: 2px; }
  .status { font-size: .8125rem; color: var(--dim); display: flex; align-items: center; gap: .4rem; }
  .dot { width: .6rem; height: .6rem; border-radius: 50%; background: var(--warn); }
  .status[data-state="ok"] .dot { background: var(--ok); }
  .why { margin-top: 1rem; padding: 1.25rem; border: 1px solid var(--border); border-radius: 14px; background: var(--card); }
  .why h2 { margin: 0 0 .5rem; font-size: 1.15rem; }
  .why-lead, .why-note { margin: 0 0 1rem; color: var(--dim); max-width: 46rem; }
  .why-note { margin: 1rem 0 0; color: var(--fg); }
  .table-wrap { overflow-x: auto; }
  table { border-collapse: collapse; width: 100%; min-width: 34rem; font-size: .9375rem; }
  th, td { text-align: left; vertical-align: top; padding: .6rem .75rem; border-top: 1px solid var(--border); }
  thead th { border-top: 0; font-size: .8125rem; color: var(--dim); text-transform: uppercase; letter-spacing: .06em; }
  tbody th { color: var(--dim); font-weight: 600; width: 7.5rem; }
  .sr-only { position: absolute; width: 1px; height: 1px; overflow: hidden; clip: rect(0 0 0 0); white-space: nowrap; }
  .deck-band {
    display: flex; flex-wrap: wrap; align-items: center; justify-content: space-between; gap: 1rem;
    margin-top: 1rem; padding: 1.25rem; border: 1px solid var(--border); border-radius: 14px;
    background: var(--card);
  }
  .deck-band h2 { margin: 0 0 .15rem; font-size: 1.15rem; }
  .deck-band p { margin: 0; color: var(--dim); }
  footer { margin-top: 2.5rem; color: var(--dim); font-size: .875rem; }
  footer a { color: inherit; }
</style>
</head>
<body>
<main>
  <p class="team">Team <strong>AirmanPrep</strong> · <a href="https://airmanprep.com" target="_blank" rel="noopener noreferrer">AirmanPrep.com</a></p>
  <h1>Planet X and SPHEREx</h1>
  <p class="lead">We are Team AirmanPrep, competing in the NASA Space Apps Challenge 2026. Two public web tools for the same challenge, built from two different angles. Open each app and its source code below.</p>

  <section class="grid" aria-label="Projects">
    <?php foreach ($projects as $p): ?>
    <article class="card">
      <h2><?= htmlspecialchars($p['name'], ENT_QUOTES) ?></h2>
      <p class="by">By <?= htmlspecialchars($p['author'], ENT_QUOTES) ?></p>
      <p><?= htmlspecialchars($p['summary'], ENT_QUOTES) ?></p>
      <?php if ($p['status_id'] !== null): ?>
      <p class="status" id="<?= $p['status_id'] ?>" role="status" aria-live="polite">
        <span class="dot" aria-hidden="true"></span><span class="status-text">Checking server…</span>
      </p>
      <?php endif; ?>
      <div class="actions">
        <a class="btn btn-primary" href="<?= htmlspecialchars($p['app'], ENT_QUOTES) ?>" target="_blank" rel="noopener noreferrer">Open app</a>
        <a class="btn" href="<?= htmlspecialchars($p['repo'], ENT_QUOTES) ?>" target="_blank" rel="noopener noreferrer">Source on GitHub</a>
      </div>
    </article>
    <?php endforeach; ?>
  </section>

  <section class="why" aria-labelledby="why-title">
    <h2 id="why-title">Why two projects for one problem?</h2>
    <p class="why-lead">The challenge asks for one thing: show a patch of sky as a short film so people can spot moving objects. There are two honest ways in. One starts from the data, the other from the person using it. We built both and kept them side by side so you can compare them.</p>
    <div class="table-wrap">
      <table>
        <caption class="sr-only">SPHEREx Blink compared with Planeta X</caption>
        <thead>
          <tr><th scope="col"></th><th scope="col">SPHEREx Blink</th><th scope="col">Planeta X</th></tr>
        </thead>
        <tbody>
          <tr><th scope="row">Starts from</th><td>The data: real SPHEREx exposures</td><td>The user: a guided hunt</td></tr>
          <tr><th scope="row">Images</th><td>Fetched live from the NASA archive for any RA/Dec</td><td>WISE cutouts from the archive and simulated SPHEREx-style regions</td></tr>
          <tr><th scope="row">You do</th><td>Play the time-lapse, switch to difference, read the spectrum</td><td>Compare epochs, click what moved, draw its path, export findings</td></tr>
          <tr><th scope="row">Good for</th><td>Measuring motion on real data</td><td>Learning the method and reporting candidates</td></tr>
          <tr><th scope="row">Built with</th><td>Rust backend, React frontend</td><td>React frontend, PHP API</td></tr>
        </tbody>
      </table>
    </div>
    <p class="why-note">SPHEREx Blink answers "what does the real archive show here?". Planeta X answers "how would I mark and report a mover?". Neither replaces the other.</p>
  </section>

  <section class="deck-band" aria-label="Presentation">
    <div>
      <h2>Presentation</h2>
      <p>Seven slides on SPHEREx Blink: the problem, the tool, the evidence and the data behind it.</p>
    </div>
    <a class="btn btn-primary" href="presentation/">View the presentation</a>
  </section>

  <footer>
    <p>Made by Team AirmanPrep. Visit our company: <a href="https://airmanprep.com" target="_blank" rel="noopener noreferrer">AirmanPrep.com</a>.</p>
    <p>Data: NASA/IPAC Infrared Science Archive (IRSA), SPHEREx spectral images. Hosting of SPHEREx Blink may take up to a minute to wake the first time; this page keeps it awake.</p>
  </footer>
</main>

<script>
(function () {
  var box = document.getElementById('status-spherex');
  if (!box) return;
  var text = box.querySelector('.status-text');
  var INTERVAL = 5 * 60 * 1000;

  function ping() {
    fetch('ping.php', { cache: 'no-store' })
      .then(function (r) { return r.json(); })
      .then(function (d) {
        if (d.ok) {
          box.dataset.state = 'ok';
          text.textContent = 'Server online';
        } else {
          box.dataset.state = 'wake';
          text.textContent = 'Server waking up, try again in a minute';
        }
      })
      .catch(function () {
        box.dataset.state = 'wake';
        text.textContent = 'Status unavailable';
      });
  }

  ping();
  setInterval(ping, INTERVAL);
})();
</script>
</body>
</html>
