// Text the tests read, held in Dart so they run in a browser, which cannot read files.
//
// `codingTaskUpdates` is the kind and shape, never the text, of every update a real agent sent in a coding task (one JSON object
// a line). `usageVectors` is the repository's `vectors/usage.json`; a test on the VM holds it to the file.

const String codingTaskUpdates =
    r'''{"kind":"user_message","rendering":"message","shape":{"sessionUpdate":"user_message","messageId":"string","content":[{"text":"string","type":"text"}]}}
{"kind":"state_update","rendering":"status","shape":{"sessionUpdate":"state_update","state":"running"}}
{"kind":"available_commands_update","rendering":"commands","shape":{"sessionUpdate":"available_commands_update","availableCommands":[{"type":"command","name":"string","description":"string"},{"type":"command","name":"string","description":"string","input":{"type":"text","hint":"string"}}]}}
{"kind":"usage_update","rendering":"usage","shape":{"sessionUpdate":"usage_update","used":"int","size":"int","_meta":{"_claude/model":"string"}}}
{"kind":"agent_message_chunk","rendering":"message","shape":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"string"},"messageId":"string"}}
{"kind":"usage_update","rendering":"usage","shape":{"sessionUpdate":"usage_update","used":"int","size":"int","_meta":{"_claude/rateLimit":{"status":"allowed","resetsAt":"int","rateLimitType":"string","overageStatus":"string","overageDisabledReason":"string","isUsingOverage":"bool","unifiedWindows":{"seven_day":{"utilization":"double","resetsAt":"int","status":"allowed"}}},"_claude/model":"string"}}}
{"kind":"session_info_update","rendering":"title","shape":{"sessionUpdate":"session_info_update","title":"string","updatedAt":"string"}}
{"kind":"tool_call_update","rendering":"toolCard","shape":{"toolCallId":"string","name":"string","rawInput":{},"status":"pending","title":"string","kind":"read","locations":[],"content":[],"sessionUpdate":"tool_call_update"}}
{"kind":"tool_call_update","rendering":"toolCard","shape":{"toolCallId":"string","rawInput":{"file_path":"string"},"title":"string","locations":[{"path":"string","line":"int"}],"sessionUpdate":"tool_call_update"}}
{"kind":"terminal_update","rendering":"terminal","shape":{"sessionUpdate":"terminal_update","terminalId":"string"}}
{"kind":"tool_call_update","rendering":"toolCard","shape":{"toolCallId":"string","name":"string","rawInput":{},"status":"pending","title":"string","kind":"execute","content":[{"type":"terminal","terminalId":"string"}],"sessionUpdate":"tool_call_update"}}
{"kind":"state_update","rendering":"status","shape":{"sessionUpdate":"state_update","state":"requires_action"}}
{"kind":"terminal_update","rendering":"terminal","shape":{"sessionUpdate":"terminal_update","terminalId":"string","command":"string"}}
{"kind":"tool_call_update","rendering":"toolCard","shape":{"toolCallId":"string","rawInput":{"command":"string"},"title":"string","sessionUpdate":"tool_call_update"}}
{"kind":"tool_call_update","rendering":"toolCard","shape":{"toolCallId":"string","rawInput":{"command":"string","description":"string"},"sessionUpdate":"tool_call_update"}}
{"kind":"tool_call_update","rendering":"toolCard","shape":{"toolCallId":"string","status":"completed","rawOutput":"string","content":[{"type":"content","content":{"type":"text","text":"string"}}],"sessionUpdate":"tool_call_update"}}
{"kind":"terminal_update","rendering":"terminal","shape":{"sessionUpdate":"terminal_update","terminalId":"string","output":{"data":"string"}}}
{"kind":"terminal_update","rendering":"terminal","shape":{"sessionUpdate":"terminal_update","terminalId":"string","exitStatus":{"exitCode":"int"}}}
{"kind":"tool_call_update","rendering":"toolCard","shape":{"toolCallId":"string","status":"failed","sessionUpdate":"tool_call_update"}}
{"kind":"tool_call_update","rendering":"toolCard","shape":{"toolCallId":"string","status":"completed","sessionUpdate":"tool_call_update"}}
{"kind":"tool_call_update","rendering":"toolCard","shape":{"toolCallId":"string","name":"string","rawInput":{},"status":"pending","title":"string","kind":"edit","locations":[],"content":[],"sessionUpdate":"tool_call_update"}}
{"kind":"tool_call_update","rendering":"toolCard","shape":{"toolCallId":"string","rawInput":{"file_path":"string"},"title":"string","locations":[{"path":"string"}],"sessionUpdate":"tool_call_update"}}
{"kind":"tool_call_update","rendering":"toolCard","shape":{"toolCallId":"string","rawInput":{"file_path":"string","old_string":"string"},"sessionUpdate":"tool_call_update"}}
{"kind":"tool_call_update","rendering":"toolCard","shape":{"toolCallId":"string","rawInput":{"replace_all":"bool","file_path":"string","old_string":"string","new_string":"string"},"sessionUpdate":"tool_call_update"}}
{"kind":"tool_call_update","rendering":"toolCard","shape":{"toolCallId":"string","content":[{"type":"diff","changes":[{"operation":"string","path":"string","fileType":"string"}],"patch":{"format":"string","text":"string"}}],"sessionUpdate":"tool_call_update"}}
{"kind":"tool_call_update","rendering":"toolCard","shape":{"toolCallId":"string","locations":[{"path":"string","line":"int"}],"sessionUpdate":"tool_call_update"}}
{"kind":"tool_call_update","rendering":"toolCard","shape":{"toolCallId":"string","status":"completed","rawOutput":"string","sessionUpdate":"tool_call_update"}}
{"kind":"usage_update","rendering":"usage","shape":{"sessionUpdate":"usage_update","used":"int","size":"int","cost":{"amount":"double","currency":"string"},"_meta":{"_claude/origin":{"kind":"human"},"_claude/model":"string"}}}
{"kind":"state_update","rendering":"status","shape":{"sessionUpdate":"state_update","state":"idle","stopReason":"end_turn","usage":{"inputTokens":"int","outputTokens":"int","cachedReadTokens":"int","cachedWriteTokens":"int","totalTokens":"int"},"_meta":{"quota":{"token_count":{"totalTokens":"int","inputTokens":"int","cachedInputTokens":"int","cachedWriteTokens":"int","outputTokens":"int","reasoningOutputTokens":"int"},"model_usage":[{"model":"string","token_count":{"totalTokens":"int","inputTokens":"int","cachedInputTokens":"int","cachedWriteTokens":"int","outputTokens":"int","reasoningOutputTokens":"int"}}]}}}}
{"kind":"tool_call_update","rendering":"toolCard","shape":{"toolCallId":"string","name":"string","rawInput":{},"status":"pending","title":"string","kind":"search","content":[],"sessionUpdate":"tool_call_update"}}
{"kind":"tool_call_update","rendering":"toolCard","shape":{"toolCallId":"string","rawInput":{"pattern":"string"},"title":"string","sessionUpdate":"tool_call_update"}}
{"kind":"tool_call_update","rendering":"toolCard","shape":{"toolCallId":"string","rawInput":{"pattern":"string","path":"string"},"title":"string","sessionUpdate":"tool_call_update"}}
{"kind":"tool_call_update","rendering":"toolCard","shape":{"toolCallId":"string","rawInput":{"pattern":"string","path":"string","output_mode":"string"},"sessionUpdate":"tool_call_update"}}
{"kind":"agent_thought_chunk","rendering":"thought","shape":{"sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":"string"},"messageId":"string"}}
{"kind":"tool_call_update","rendering":"toolCard","shape":{"toolCallId":"string","status":"cancelled","rawOutput":"string","_meta":{"claudeCode":{"nonExecutionKind":"string"}},"content":[{"type":"content","content":{"type":"text","text":"string"}}],"sessionUpdate":"tool_call_update"}}
{"kind":"tool_call_update","rendering":"toolCard","shape":{"toolCallId":"string","status":"cancelled","content":[{"type":"content","content":{"type":"text","text":"string"}}],"sessionUpdate":"tool_call_update"}}
{"kind":"state_update","rendering":"status","shape":{"sessionUpdate":"state_update","state":"idle","stopReason":"cancelled","usage":{"inputTokens":"int","outputTokens":"int","cachedReadTokens":"int","cachedWriteTokens":"int","totalTokens":"int"},"_meta":{"quota":{"token_count":{"totalTokens":"int","inputTokens":"int","cachedInputTokens":"int","cachedWriteTokens":"int","outputTokens":"int","reasoningOutputTokens":"int"},"model_usage":[{"model":"string","token_count":{"totalTokens":"int","inputTokens":"int","cachedInputTokens":"int","cachedWriteTokens":"int","outputTokens":"int","reasoningOutputTokens":"int"}}]}}}}
{"kind":"tool_call_update","rendering":"toolCard","shape":{"toolCallId":"string","rawInput":{"file_path":"string","content":"string"},"sessionUpdate":"tool_call_update"}}
{"kind":"tool_call_update","rendering":"toolCard","shape":{"toolCallId":"string","rawInput":{"file_path":"string","old_string":"string","new_string":"string"},"sessionUpdate":"tool_call_update"}}
{"kind":"user_message","rendering":"message","shape":{"sessionUpdate":"user_message","messageId":"string","content":[{"text":"string","type":"text"},{"resource":{"text":"string","uri":"string"},"type":"resource"}]}}
{"kind":"terminal_update","rendering":"terminal","shape":{"sessionUpdate":"terminal_update","terminalId":"string","exitStatus":{}}}
{"kind":"tool_call_update","rendering":"toolCard","shape":{"toolCallId":"string","status":"failed","_meta":{"claudeCode":{"nonExecutionKind":"string"}},"sessionUpdate":"tool_call_update"}}
{"kind":"tool_call_update","rendering":"toolCard","shape":{"toolCallId":"string","name":"string","rawInput":{},"status":"pending","title":"string","kind":"search","locations":[],"content":[],"sessionUpdate":"tool_call_update"}}
{"kind":"tool_call_update","rendering":"toolCard","shape":{"toolCallId":"string","status":"failed","rawOutput":"string","_meta":{"claudeCode":{"nonExecutionKind":"string"}},"content":[{"type":"content","content":{"type":"text","text":"string"}}],"sessionUpdate":"tool_call_update"}}
{"kind":"tool_call_update","rendering":"toolCard","shape":{"toolCallId":"string","rawInput":{"pattern":"string","path":"string"},"title":"string","locations":[{"path":"string"}],"sessionUpdate":"tool_call_update"}}
{"kind":"user_message","rendering":"message","shape":{"messageId":"string","content":[],"sessionUpdate":"user_message"}}
{"kind":"user_message_chunk","rendering":"message","shape":{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"string"},"messageId":"string"}}
{"kind":"agent_message","rendering":"message","shape":{"messageId":"string","content":[],"sessionUpdate":"agent_message"}}
{"kind":"tool_call_update","rendering":"toolCard","shape":{"locations":"null","rawOutput":"null","toolCallId":"string","name":"string","rawInput":{"command":"string","description":"string"},"status":"pending","title":"string","kind":"execute","content":[{"type":"terminal","terminalId":"string"}],"sessionUpdate":"tool_call_update","_meta":"null"}}
{"kind":"tool_call_update","rendering":"toolCard","shape":{"toolCallId":"string","status":"failed","content":[{"type":"terminal","terminalId":"string"}],"sessionUpdate":"tool_call_update"}}
{"kind":"tool_call_update","rendering":"toolCard","shape":{"rawOutput":"null","toolCallId":"string","name":"string","rawInput":{"file_path":"string"},"status":"pending","title":"string","kind":"read","locations":[{"path":"string","line":"int"}],"content":[],"sessionUpdate":"tool_call_update","_meta":"null"}}
{"kind":"tool_call_update","rendering":"toolCard","shape":{"toolCallId":"string","status":"failed","rawOutput":"string","content":[{"type":"content","content":{"type":"text","text":"string"}}],"sessionUpdate":"tool_call_update"}}
{"kind":"tool_call_update","rendering":"toolCard","shape":{"rawOutput":"null","toolCallId":"string","name":"string","rawInput":{"pattern":"string","path":"string"},"status":"pending","title":"string","kind":"search","locations":[{"path":"string"}],"content":[],"sessionUpdate":"tool_call_update","_meta":"null"}}
{"kind":"tool_call_update","rendering":"toolCard","shape":{"rawOutput":"null","toolCallId":"string","name":"string","rawInput":{"replace_all":"bool","file_path":"string","old_string":"string","new_string":"string"},"status":"pending","title":"string","kind":"edit","locations":[{"path":"string"}],"content":[],"sessionUpdate":"tool_call_update","_meta":"null"}}
{"kind":"tool_call_update","rendering":"toolCard","shape":{"toolCallId":"string","status":"completed","content":[{"type":"terminal","terminalId":"string"}],"sessionUpdate":"tool_call_update"}}
{"kind":"tool_call_update","rendering":"toolCard","shape":{"locations":"null","rawOutput":"null","toolCallId":"string","name":"string","rawInput":{"pattern":"string","path":"string","output_mode":"string"},"status":"pending","title":"string","kind":"search","content":[],"sessionUpdate":"tool_call_update","_meta":"null"}}
{"kind":"agent_thought","rendering":"thought","shape":{"messageId":"string","content":[],"sessionUpdate":"agent_thought"}}
{"kind":"state_update","rendering":"status","shape":{"sessionUpdate":"state_update","state":"idle"}}
{"kind":"tool_call_update","rendering":"toolCard","shape":{"toolCallId":"string","rawInput":{"pattern":"string","output_mode":"string"},"sessionUpdate":"tool_call_update"}}
{"kind":"tool_call_update","rendering":"toolCard","shape":{"toolCallId":"string","rawInput":{"command":"string","description":"string"},"title":"string","sessionUpdate":"tool_call_update"}}
{"kind":"tool_call_update","rendering":"toolCard","shape":{"locations":"null","rawOutput":"null","toolCallId":"string","name":"string","rawInput":{"pattern":"string","output_mode":"string"},"status":"pending","title":"string","kind":"search","content":[],"sessionUpdate":"tool_call_update","_meta":"null"}}
{"kind":"tool_call_update","rendering":"toolCard","shape":{"rawOutput":"null","toolCallId":"string","name":"string","rawInput":{"file_path":"string","content":"string"},"status":"pending","title":"string","kind":"edit","locations":[{"path":"string"}],"content":[],"sessionUpdate":"tool_call_update","_meta":"null"}}
{"kind":"tool_call_content_chunk","rendering":"toolCard","shape":{"sessionUpdate":"tool_call_content_chunk","toolCallId":"string","content":{"type":"content","content":{"type":"text","text":"string"}}}}
{"kind":"notice","rendering":"notice","shape":{"sessionUpdate":"notice","severity":"string","title":"string"}}
{"kind":"compaction_summary_chunk","rendering":"compaction","shape":{"sessionUpdate":"compaction_summary_chunk","compactionId":"string","content":{"type":"text","text":"string"}}}
{"kind":"compaction_update","rendering":"compaction","shape":{"sessionUpdate":"compaction_update","compactionId":"string","status":"completed"}}
{"kind":"plan_update","rendering":"plan","shape":{"sessionUpdate":"plan_update","plan":{"type":"items","planId":"string","entries":[{"content":"string","status":"completed","priority":"high"},{"content":"string","status":"in_progress","priority":"high"},{"content":"string","status":"cancelled","priority":"low"},{"content":"string","status":"pending","priority":"medium"}]}}}
{"kind":"user_message_chunk","rendering":"message","shape":{"sessionUpdate":"user_message_chunk","content":{"type":"resource","resource":{"uri":"string","text":"string"}}}}
''';

const String usageVectors = r'''{
  "about": "The usage model (`coqui_acp_api::usage`) as every reader and writer must spell it, in Rust here and in Dart for Canoa's app. `roundtrip` is a report written in its one canonical form: an absent optional left out (a reader also takes null), a list always written, even empty, a length as [seconds, nanoseconds], a time in Unix seconds, no float. Reading it and writing it again must give the same JSON. `lenient` is what a reader is given that a newer writer could send: a unit, status, source, refill or coverage it does not know reads as its `other` (a unit, status or source keeps the name it was sent under, a refill or coverage keeps none) and the whole report is still read; `reads_as` is what is written back. `refused` is a request that names a key it does not declare. `fraction` is `UsageReport::fraction_permille` of the bucket, in thousandths and at most 1000, or null: used over limit, else limit minus remaining over limit. `resets_in` is `Bucket::resets_in(now)` in seconds, null for a bucket that is not a fixed window with a known end, and 0 once the end has passed.",
  "roundtrip": [
    {
      "name": "a harness that reports no usage",
      "report": {
        "supported": false,
        "unsupported_because": "this harness reports no usage",
        "sources": [],
        "revision": 0,
        "observed_at": 0,
        "buckets": []
      }
    },
    {
      "name": "supported, nothing reported yet",
      "report": {
        "supported": true,
        "sources": [
          "reported"
        ],
        "account": "pool/alice",
        "revision": 0,
        "observed_at": 0,
        "buckets": []
      }
    },
    {
      "name": "Claude's two windows, one covering a model",
      "report": {
        "supported": true,
        "sources": [
          "reported",
          "inferred"
        ],
        "account": "pool/alice",
        "revision": 7,
        "observed_at": 1788900000,
        "buckets": [
          {
            "id": "five_hour",
            "label": "5 hours",
            "models": {
              "kind": "all"
            },
            "unit": "permille",
            "used": 620,
            "limit": 1000,
            "refill": {
              "kind": "fixed_window",
              "length": [
                18000,
                0
              ],
              "resets_at": 1788915000
            },
            "status": "ok",
            "observed_at": 1788900000,
            "source": "reported"
          },
          {
            "id": "seven_day_opus",
            "models": {
              "kind": "only",
              "models": [
                "opus"
              ]
            },
            "unit": "permille",
            "used": 810,
            "limit": 1000,
            "refill": {
              "kind": "fixed_window",
              "length": [
                604800,
                0
              ],
              "resets_at": 1789300000
            },
            "status": "warning",
            "observed_at": 1788899000,
            "source": "reported"
          }
        ]
      }
    },
    {
      "name": "a token bucket in requests, and a balance in money that never refills",
      "report": {
        "supported": true,
        "sources": [
          "reported"
        ],
        "revision": 3,
        "observed_at": 1788900100,
        "buckets": [
          {
            "id": "rpm",
            "label": "Requests a minute",
            "models": {
              "kind": "unknown"
            },
            "unit": "requests",
            "limit": 60,
            "remaining": 41,
            "refill": {
              "kind": "token_bucket",
              "amount": 60,
              "per": [
                60,
                0
              ],
              "max": 60
            },
            "status": "ok",
            "observed_at": 1788900100,
            "source": "reported"
          },
          {
            "id": "credit",
            "label": "Credit",
            "models": {
              "kind": "all"
            },
            "unit": "micro_usd",
            "used": 3500000,
            "limit": 5000000,
            "remaining": 1500000,
            "refill": {
              "kind": "none"
            },
            "status": "ok",
            "observed_at": 1788900050,
            "source": "reported"
          },
          {
            "id": "tpm",
            "models": {
              "kind": "all"
            },
            "unit": "tokens",
            "used": 12000,
            "limit": 400000,
            "refill": {
              "kind": "token_bucket",
              "amount": 400000,
              "per": [
                60,
                500000000
              ],
              "max": 400000
            },
            "status": "unknown",
            "observed_at": 1788900100,
            "source": "reported"
          }
        ]
      }
    },
    {
      "name": "a window whose length and end are not known, inferred from a refusal",
      "report": {
        "supported": true,
        "sources": [
          "inferred"
        ],
        "account": "pool/bob",
        "revision": 1,
        "observed_at": 1788900200,
        "buckets": [
          {
            "id": "inferred",
            "models": {
              "kind": "unknown"
            },
            "unit": "permille",
            "refill": {
              "kind": "fixed_window"
            },
            "status": "exhausted",
            "observed_at": 1788900200,
            "source": "inferred",
            "evidence": "RESOURCE_EXHAUSTED"
          }
        ],
        "limit": {
          "bucket": "inferred",
          "source": "inferred",
          "text": "Error 429: RESOURCE_EXHAUSTED",
          "observed_at": 1788900200
        }
      }
    },
    {
      "name": "a refusal with a reset and a retry-after, and a session's spend by model",
      "report": {
        "supported": true,
        "sources": [
          "reported"
        ],
        "account": "pool/alice",
        "revision": 12,
        "observed_at": 1788900300,
        "buckets": [
          {
            "id": "codex:primary",
            "models": {
              "kind": "only",
              "models": [
                "gpt-5-codex",
                "gpt-5"
              ]
            },
            "unit": "permille",
            "used": 1000,
            "limit": 1000,
            "refill": {
              "kind": "fixed_window",
              "length": [
                18000,
                0
              ],
              "resets_at": 1788910000
            },
            "status": "exhausted",
            "observed_at": 1788900300,
            "source": "reported"
          }
        ],
        "limit": {
          "bucket": "codex:primary",
          "resets_at": 1788910000,
          "retry_after": [
            90,
            0
          ],
          "source": "reported",
          "text": "You have hit your usage limit.",
          "observed_at": 1788900300
        },
        "spend": {
          "session_id": "0f8fad5b-d9cb-469f-a165-70867728950e",
          "input": 1200,
          "output": 340,
          "cache_read": 9000,
          "cache_write": 800,
          "reasoning": 120,
          "cost_micro_usd": 48500,
          "by_model": [
            {
              "model": "gpt-5-codex",
              "input": 1000,
              "output": 300,
              "cache_read": 8000,
              "cache_write": 700,
              "cost_micro_usd": 40000
            },
            {
              "model": "gpt-5",
              "input": 200,
              "output": 40,
              "cache_read": 1000,
              "cache_write": 100
            }
          ]
        }
      }
    },
    {
      "name": "a spend whose cost is unknown stays unknown, never zero",
      "report": {
        "supported": true,
        "sources": [
          "reported"
        ],
        "revision": 2,
        "observed_at": 0,
        "buckets": [],
        "spend": {
          "session_id": "7c9e6679-7425-40de-944b-e07fc1f90ae7",
          "input": 100,
          "output": 40,
          "cache_read": 20,
          "cache_write": 10,
          "by_model": []
        }
      }
    }
  ],
  "lenient": [
    {
      "name": "a unit, status and source a newer writer named are kept under their names",
      "read": {
        "supported": true,
        "unsupported_because": null,
        "sources": [
          "reported",
          "scraped"
        ],
        "account": null,
        "revision": 1,
        "observed_at": 5,
        "buckets": [
          {
            "id": "b",
            "label": null,
            "models": {
              "kind": "all"
            },
            "unit": "credits",
            "used": 1,
            "limit": 2,
            "remaining": null,
            "refill": {
              "kind": "none"
            },
            "status": "throttled",
            "observed_at": 5,
            "source": "estimated",
            "evidence": null
          }
        ],
        "limit": null,
        "spend": null
      },
      "reads_as": {
        "supported": true,
        "sources": [
          "reported",
          "scraped"
        ],
        "revision": 1,
        "observed_at": 5,
        "buckets": [
          {
            "id": "b",
            "models": {
              "kind": "all"
            },
            "unit": "credits",
            "used": 1,
            "limit": 2,
            "refill": {
              "kind": "none"
            },
            "status": "throttled",
            "observed_at": 5,
            "source": "estimated"
          }
        ]
      },
      "unknown": {
        "unit": "other",
        "status": "other",
        "source": "other"
      }
    },
    {
      "name": "a refill and a coverage of a kind no reader knows read as other, and the bucket is still read",
      "read": {
        "supported": true,
        "unsupported_because": null,
        "sources": [
          "reported"
        ],
        "account": null,
        "revision": 1,
        "observed_at": 5,
        "buckets": [
          {
            "id": "b",
            "label": null,
            "models": {
              "kind": "tier",
              "tier": "pro"
            },
            "unit": "tokens",
            "used": 1,
            "limit": 2,
            "remaining": null,
            "refill": {
              "kind": "leaky_bucket",
              "rate": 3
            },
            "status": "ok",
            "observed_at": 5,
            "source": "reported",
            "evidence": null
          }
        ],
        "limit": null,
        "spend": null
      },
      "reads_as": {
        "supported": true,
        "sources": [
          "reported"
        ],
        "revision": 1,
        "observed_at": 5,
        "buckets": [
          {
            "id": "b",
            "models": {
              "kind": "other"
            },
            "unit": "tokens",
            "used": 1,
            "limit": 2,
            "refill": {
              "kind": "other"
            },
            "status": "ok",
            "observed_at": 5,
            "source": "reported"
          }
        ]
      },
      "unknown": {
        "refill": "other",
        "models": "other"
      }
    },
    {
      "name": "a report with only what it must say is read, and a list or an optional left out is empty or absent",
      "read": {
        "supported": false,
        "revision": 0,
        "observed_at": 0
      },
      "reads_as": {
        "supported": false,
        "sources": [],
        "revision": 0,
        "observed_at": 0,
        "buckets": []
      },
      "unknown": {}
    }
  ],
  "refused": [
    {
      "name": "a query that names a key it does not declare",
      "type": "UsageQuery",
      "json": {
        "account": "pool/alice",
        "refresh": true
      }
    },
    {
      "name": "a start request that names a key it does not declare",
      "type": "StartRequest",
      "json": {
        "account": "pool/alice",
        "tier": "pro"
      }
    }
  ],
  "fraction": [
    {
      "name": "used over limit",
      "bucket": {
        "used": 620,
        "limit": 1000
      },
      "permille": 620
    },
    {
      "name": "used over limit, rounded down",
      "bucket": {
        "used": 1,
        "limit": 3
      },
      "permille": 333
    },
    {
      "name": "limit minus remaining over limit",
      "bucket": {
        "remaining": 1500000,
        "limit": 5000000
      },
      "permille": 700
    },
    {
      "name": "used wins over remaining",
      "bucket": {
        "used": 100,
        "remaining": 100,
        "limit": 1000
      },
      "permille": 100
    },
    {
      "name": "over the limit is a whole",
      "bucket": {
        "used": 5000,
        "limit": 1000
      },
      "permille": 1000
    },
    {
      "name": "remaining past the limit is nothing used",
      "bucket": {
        "remaining": 90,
        "limit": 60
      },
      "permille": 0
    },
    {
      "name": "a limit of zero says nothing",
      "bucket": {
        "used": 1,
        "limit": 0
      },
      "permille": null
    },
    {
      "name": "no limit says nothing",
      "bucket": {
        "used": 100
      },
      "permille": null
    },
    {
      "name": "no figure says nothing",
      "bucket": {
        "limit": 1000
      },
      "permille": null
    },
    {
      "name": "counts as large as a double holds exactly",
      "bucket": {
        "used": 9007199254740991,
        "limit": 9007199254740991
      },
      "permille": 1000
    }
  ],
  "resets_in": [
    {
      "name": "a fixed window with its end ahead",
      "refill": {
        "kind": "fixed_window",
        "length": null,
        "resets_at": 1000
      },
      "now": 400,
      "seconds": 600
    },
    {
      "name": "a fixed window whose end has passed",
      "refill": {
        "kind": "fixed_window",
        "length": null,
        "resets_at": 1000
      },
      "now": 1500,
      "seconds": 0
    },
    {
      "name": "a fixed window with no known end",
      "refill": {
        "kind": "fixed_window",
        "length": [
          18000,
          0
        ],
        "resets_at": null
      },
      "now": 400,
      "seconds": null
    },
    {
      "name": "a token bucket refills continuously",
      "refill": {
        "kind": "token_bucket",
        "amount": 60,
        "per": [
          60,
          0
        ],
        "max": 60
      },
      "now": 400,
      "seconds": null
    },
    {
      "name": "a balance never refills",
      "refill": {
        "kind": "none"
      },
      "now": 400,
      "seconds": null
    }
  ]
}
''';
