# Drafting rules with a model

lastmile never uses a model to read your messages. A model is only ever asked
to **write a rule**, from one message no rule matched; a person reviews and
saves the rule, and from then on it is plain, local pattern matching.

Drafting is off until a model is configured. It then shows up for admins as
*Draft with a model* on inbox messages.

## What is sent

1. **Redaction.** The message is redacted before anything else happens:
   your household's names, addresses, postcodes, phone numbers and emails
   (*Settings → Household*) are replaced, ignoring case and accents, and so is
   anything with a recognisable personal format: emails, phone numbers,
   postcodes, NIFs, IBANs and card numbers (checksums validated), the path
   tokens and query values of URLs (the host stays), and the name in a
   greeting (*Olá Ana*). They become numbered placeholders that keep the text's
   shape: `‹NAME_1›`, `‹ADDRESS_1›`, `‹PHONE_1›`.
   Tracking numbers, delivery and pickup codes, shop and courier names and
   dates stay: they are what the rule has to find. Tick *also hide the code* to
   replace the code with `‹CODE:dddd›`.
2. **You see it first.** The draft page shows the original next to the redacted
   text, which you can edit. Nothing is sent until you click *Send and draft*.
3. **A last check.** Whatever is about to be sent is scanned once more for the
   household's values: if one is in it (you typed it back, or it was spelled in
   a way the patterns missed), the request is refused.

Redaction is a net, not a guarantee: a name you didn't list, or an order number
that looks like a phone number, can slip through or be replaced wrongly.
Read the redacted text. An analyzer service can catch more names
([below](#finding-more-names-with-presidio)).

## What happens to the answer

The model answers with a rule as a JSON object. lastmile then checks it
*locally* before showing it: the rule compiles within its limits, it matches the
message (the redacted one, and the original, which is only ever run here), it
reads a tracking number or order reference, and it doesn't change what any other
rule's saved samples read. If a check fails, the problems (never the message)
go back to the model, up to three more times. You get the best draft and
what is still wrong with it, in the editor, where it is only saved when you
save it. The message becomes the rule's first sample, stored redacted unless
you tick *Store the sample as it is*.

## Configuring the model

Anything that speaks OpenAI's API works: OpenAI itself, or a self-hosted server.

| Flag | Environment | |
|---|---|---|
| `--llm-model` | `LASTMILE_LLM_MODEL` | The model name. Setting it turns drafting on |
| `--llm-base-url` | `LASTMILE_LLM_BASE_URL` | Default `https://api.openai.com/v1`. Fixed by the deployment, never taken from a request |
| `--llm-api-key-file` | `LASTMILE_LLM_API_KEY_FILE` | File holding the API key. Optional for a server without authentication |
| `--llm-api` | `LASTMILE_LLM_API` | `chat` (Chat Completions, the default and what self-hosted servers speak) or `responses` (the Responses API) |
| `--llm-structured` | `LASTMILE_LLM_STRUCTURED` | `schema` (strict JSON Schema, the default), `json` (any JSON object) or `none` (the prompt asks for JSON) |
| `--llm-timeout` | `LASTMILE_LLM_TIMEOUT` | Per request, default `120s` |

### OpenAI

```sh
lastmile --llm-model <model> --llm-api-key-file /etc/lastmile/llm/api-key
```

If the model you want is only offered on the Responses API (some Codex models
are), add `--llm-api responses`. Use a model that supports structured outputs;
if it doesn't, try `--llm-structured json`.

### Self-hosted

Point `--llm-base-url` at the server's OpenAI-compatible endpoint. The key is
optional.

```sh
# Ollama
lastmile --llm-model qwen3-coder --llm-base-url http://ollama.ai.svc:11434/v1 --llm-structured json
# vLLM
lastmile --llm-model Qwen/Qwen3-Coder-30B-A3B-Instruct --llm-base-url http://vllm.ai.svc:8000/v1
# llama.cpp (llama-server) or LM Studio
lastmile --llm-model local --llm-base-url http://llama.ai.svc:8080/v1 --llm-structured json
```

Servers differ in how much of *structured outputs* they support: if drafts come
back as unparsable text, use `--llm-structured json` or `none` (lastmile then
puts the schema in the prompt and reads the JSON out of the reply, even inside
code fences). Small models need the retries more; the local checks are what keep
a bad draft from being saved.

Because nothing personal leaves the cluster with a self-hosted model, you may
care less about the redaction there. It still runs: it is also what makes
samples safe to share.

### Helm

```yaml
llm:
  model: <model>
  baseUrl: https://api.openai.com/v1   # or http://ollama.ai.svc:11434/v1
  apiKey:
    existingSecret: lastmile-llm       # a Secret with the key `api-key`; leave empty if none
```

## Finding more names with Presidio

The household list only knows the people you wrote down. A
[Presidio](https://microsoft.github.io/presidio/) analyzer, a small service
that runs a language model over the text, also finds a name or place that isn't
on it (the neighbour who collects for you, a pickup point's street). It is
**optional and off by default**.

With `--presidio-url` set, the draft page sends the *original* message to the
analyzer and replaces what it finds with the same placeholders (a second name
becomes `‹NAME_2›`). The page says so, and where the analyzer is. Because it is
sent the original, **it should run in your cluster**, which is what the chart's
option below does; pointing it at a hosted service would send that service the
message you are protecting.

| Flag | |
|---|---|
| `--presidio-url` | Base URL of the analyzer (before `/analyze`). Turns it on |
| `--presidio-language` | Default `en` |
| `--presidio-entities` | Default `PERSON,LOCATION`: names, and places as addresses |
| `--presidio-score` | Lowest score acted on, default `0.5` |
| `--presidio-timeout` | Default `10s` |

What it adds is a suggestion, in the same editable text as everything else, and
it changes nothing else: the household values and patterns still apply and win
where they overlap, and the last check still refuses a request that holds a
household value. If the analyzer is down or slow, the page says so and shows
the redaction without it.

**What to expect.** The stock image knows English only (spaCy
`en_core_web_lg`), and asking it for another language fails. On Portuguese
messages it still finds the names, but not always the streets and cities, and
it can take a greeting into the name (`Olá Joana Ferreira` becomes `‹NAME_1›`).
For Portuguese proper you'd build an analyzer image with a Portuguese NLP model
and configuration (see Presidio's docs on
[languages](https://microsoft.github.io/presidio/analyzer/languages/)), and set
`presidio.image` and `presidio.language`.

### Helm

```yaml
presidio:
  enabled: true     # starts the analyzer with the release, reachable only inside the cluster
  # url: http://presidio.tools.svc:3000   # or use one that already runs (and leave enabled off)
```

The pod runs as a non-root user with a read-only filesystem and no
capabilities, needs about 1 GiB of memory, and takes about a minute to load its
model before it is ready. Nothing is stored.

## Metrics

`lastmile_llm_requests_total{result}` (`ok`, `error`) and
`lastmile_llm_tokens_total{kind}` (`input`, `output`);
`lastmile_analyzer_requests_total{result}` for the analyzer.
