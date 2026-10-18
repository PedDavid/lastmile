# Sending messages from an iPhone

iOS doesn't let web apps receive shares, so a Shortcut does it: it takes the
text from the share sheet (in Messages, Mail or WhatsApp), sends it to
lastmile and shows what lastmile made of it as a notification:

> CTT RR123456789PT – ready for pickup, code 482913

or *No rule matched – in the inbox*.

## 1. Get a token

In lastmile, open **Settings → Devices**, name the phone (*Ana's iPhone*)
and **Add device**. Copy the token (`lm_…`): it is shown only once. Each
token belongs to whoever created it; revoke it there if the phone is lost.

## 2. Make the Shortcut

In the Shortcuts app, create a new shortcut named *lastmile* and add:

1. **Receive** *Text* input from *Share Sheet*. If there is no input:
   *Continue* (so it also works with the clipboard when run by hand) and
   add **Get Clipboard** under it.
2. **Get Contents of URL**
   - URL: `https://<your lastmile>/api/ingest`
   - Method: **POST**
   - Headers: `Authorization` = `Bearer lm_…` (your token)
   - Request Body: **JSON**, with one field `text` (Text) = *Shortcut Input*
3. **Get Dictionary Value** for `summary` in *Contents of URL*.
4. **Show Notification** with the *Dictionary Value*.

Turn on **Show in Share Sheet** in the shortcut's details.

## What the API takes and returns

```http
POST /api/ingest
Authorization: Bearer lm_…
Content-Type: application/json

{"text": "CTT: A sua encomenda RR123456789PT está disponível…"}
```

```json
{
  "result": "matched",
  "summary": "CTT RR123456789PT – ready for pickup, code 482913",
  "message_id": 12,
  "package": { "id": 3, "url": "https://parcels.example.com/packages/3" }
}
```

`result` is `matched`, `unmatched` or `error` (a rule matched but couldn't
read the tracking number: the message waits in the inbox). An optional
`"channel": "api"` marks messages that weren't shared by a person. Bodies are
limited to 256 KiB and each token to 30 messages per 10 minutes. The
endpoint needs no login, so it must be reachable from the phone wherever it
is (through the usual Ingress or a VPN).
