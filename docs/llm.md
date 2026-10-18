# Drafting rules with a model

lastmile never uses a model to read your messages. A model is only ever asked
to **write a rule**, from one message no rule matched; a person reviews and
saves the rule, and from then on it is plain, local pattern matching.

Drafting is off until a model is configured. It then shows up for admins as
*Draft with a model* on inbox messages.

## What is sent

1. **Only what you read.** The draft page shows the message as it is, in a text
   box you can edit, and nothing is sent until you click *Send and draft*. What
   goes out is that text, the channel, the sender (for emails) and the
   description you add, plus up to three of your existing rules for the same
   courier, as examples.
2. **Take out what is personal.** A message from a courier usually names you,
   your address and your phone number. The rule doesn't need them: delete them
   (or replace them with anything) in the text box. The message in the inbox
   stays as it is, and so does the sample the rule keeps.
3. **Nothing else.** If a draft fails the local checks, the retry sends back
   the model's own draft and the problems found, which name fields and rules and
   never quote the message.

With a self-hosted model nothing leaves your network, so there is little to take
out. With a hosted one, what you leave in is sent.

## What happens to the answer

The model answers with a rule as a JSON object. lastmile then checks it
*locally* before showing it: the rule compiles within its limits, it matches the
message, it reads a tracking number or order reference, and it doesn't change
what any other rule's saved samples read. If a check fails, the problems (never
the message) go back to the model, up to three more times. You get the best
draft and what is still wrong with it, in the editor, where it is only saved
when you save it. The message becomes the rule's first sample, stored as it
came.

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

### Helm

```yaml
llm:
  model: <model>
  baseUrl: https://api.openai.com/v1   # or http://ollama.ai.svc:11434/v1
  apiKey:
    existingSecret: lastmile-llm       # a Secret with the key `api-key`; leave empty if none
```

## Metrics

`lastmile_llm_requests_total{result}` (`ok`, `error`) and
`lastmile_llm_tokens_total{kind}` (`input`, `output`).
