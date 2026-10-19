# Checking packages with the courier

**Experimental, and off by default.** lastmile can open a courier's tracking
page for the packages you are expecting, and add what it says to their
timeline and status. Today that is CTT (the `XX#########PT` tracking numbers).
Messages remain the main source of status: this fills the gaps between them.

## Why a browser

CTT's tracking page (`https://www.ctt.pt/t/<number>`) is a web app: its HTML is
an empty shell, and the page's own scripts fetch and draw the events. Nothing
can be read from it without running those scripts, so lastmile drives a
headless Chromium: it opens the page, waits until the page has drawn its
events and they have stopped changing, and reads the text a visitor would see.

## What it does

- Every `--tracking-interval` (default 2 hours), each active package that has a
  CTT tracking number and is not delivered is checked, **one at a time with a
  pause between them**. The first check is a minute after start.
- The dated lines on the page become events on the package's timeline (with
  source *courier*), once each. What CTT says is mapped to the app's six
  states; wordings that are not recognised stay on the timeline without
  changing the status.
- The status changes only like it does for a message: for news newer than the
  current status, forwards (or into or out of *exception*). A status you set by
  hand is kept until the page has something newer.
- The expected day on the page becomes the package's ETA, and the package gets
  a link to the page if it had none.
- Nothing else leaves: only CTT's tracking page is opened, with the tracking
  number in its address, as it is when you click the link yourself. The page
  loads whatever CTT's page loads. The browser introduces itself as
  `lastmile (+https://github.com/PedDavid/lastmile; …)`.

## Setting it up

| Flag | |
|---|---|
| `--browser-url` | The browser's DevTools endpoint (`http://host:9222`). Turns it on |
| `--browser-timeout` | How long one page may take to show its events, default `60s` |
| `--tracking-interval` | How often packages are checked, default `2h` |

Any Chromium that exposes the DevTools protocol will do; the chart runs
[`chromedp/headless-shell`](https://hub.docker.com/r/chromedp/headless-shell):

```yaml
browser:
  enabled: true      # starts Chromium with the release, reachable only inside the cluster
  # url: http://chromium.tools.svc:9222   # or use one that already runs (and leave enabled off)
  interval: 2h
```

The pod runs as a non-root user with a read-only filesystem and no
capabilities, and wants about 256 MiB to 1 GiB. **Keep the browser inside the
cluster:** whoever can reach its DevTools port controls it.

Chromium only answers its DevTools endpoint when the request's `Host` is
`localhost` or an IP address, so lastmile resolves a service name to its
address before it connects.

## Seeing what it does

`lastmile_courier_checks_total{courier,result}` counts each check as `ok`,
`empty` (the page loaded but had no events lastmile could read) or `error` (the
browser or the page failed). The log has a line for each package that had news.

## What is not verified

The reading of CTT's page is written against the text a tracking page shows
(dated lines, each with what happened), and tested on made-up pages of that
shape, not on CTT's real page, which was not available when this was written.
If checks come back `empty`, CTT's layout or wording differs. Run with
`RUST_LOG=lastmile=debug` to have the text that was read written to the log,
and the new wording can be added to `src/couriers/ctt.rs`. The log then
contains the page's text, which can include the delivery address.
