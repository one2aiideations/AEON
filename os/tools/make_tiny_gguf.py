"""
Write a tiny random-weight llama-architecture GGUF (about 1 MB) for boot and CI tests.
The output is gibberish by design; the test only checks that the full path
(aeond -> llama-server -> model -> reply) works offline.

    python os/tools/make_tiny_gguf.py OUT.gguf [--gguf-py PATH_TO_llama.cpp/gguf-py]
"""
import argparse, sys
from pathlib import Path

import numpy as np

ap = argparse.ArgumentParser()
ap.add_argument("out")
ap.add_argument("--gguf-py", default=None, help="llama.cpp/gguf-py folder if the gguf package is not installed")
ap.add_argument("--seed", type=int, default=0)
a = ap.parse_args()
if a.gguf_py:
    sys.path.insert(0, a.gguf_py)
import gguf  # noqa: E402

V_WORDS = ["▁the", "▁a", "▁and", "▁model", "▁AEON", "▁is", "▁ready", "▁hello", "▁world", "▁.", "▁answer"]
tokens = ["<unk>", "<s>", "</s>"] + [f"<0x{i:02X}>" for i in range(256)] + V_WORDS
types = [2, 3, 3] + [6] * 256 + [1] * len(V_WORDS)   # unknown, control, control, byte..., normal
scores = [0.0, 0.0, 0.0] + [-1000.0] * 256 + [-float(i) for i in range(len(V_WORDS))]
n_vocab, n_embd, n_head, n_layer, n_ff, n_ctx = len(tokens), 64, 4, 2, 128, 512

rng = np.random.default_rng(a.seed)
def w(*shape):
    return (rng.standard_normal(shape) * 0.05).astype(np.float32)

wr = gguf.GGUFWriter(a.out, "llama")
wr.add_name("aeon-tiny-test")
wr.add_context_length(n_ctx)
wr.add_embedding_length(n_embd)
wr.add_block_count(n_layer)
wr.add_feed_forward_length(n_ff)
wr.add_head_count(n_head)
wr.add_head_count_kv(n_head)
wr.add_rope_dimension_count(n_embd // n_head)
wr.add_layer_norm_rms_eps(1e-5)
wr.add_file_type(gguf.LlamaFileType.ALL_F32)
wr.add_tokenizer_model("llama")
wr.add_token_list(tokens)
wr.add_token_scores(scores)
wr.add_token_types(types)
wr.add_bos_token_id(1)
wr.add_eos_token_id(2)
wr.add_unk_token_id(0)
wr.add_add_bos_token(True)
wr.add_add_eos_token(False)

wr.add_tensor("token_embd.weight", w(n_vocab, n_embd))
for i in range(n_layer):
    wr.add_tensor(f"blk.{i}.attn_norm.weight", np.ones(n_embd, np.float32))
    for n in ("attn_q", "attn_k", "attn_v", "attn_output"):
        wr.add_tensor(f"blk.{i}.{n}.weight", w(n_embd, n_embd))
    wr.add_tensor(f"blk.{i}.ffn_norm.weight", np.ones(n_embd, np.float32))
    wr.add_tensor(f"blk.{i}.ffn_gate.weight", w(n_ff, n_embd))
    wr.add_tensor(f"blk.{i}.ffn_up.weight", w(n_ff, n_embd))
    wr.add_tensor(f"blk.{i}.ffn_down.weight", w(n_embd, n_ff))
wr.add_tensor("output_norm.weight", np.ones(n_embd, np.float32))
wr.add_tensor("output.weight", w(n_vocab, n_embd))

wr.write_header_to_file()
wr.write_kv_data_to_file()
wr.write_tensors_to_file()
wr.close()
print(f"wrote {a.out} ({Path(a.out).stat().st_size // 1024} KB, vocab {n_vocab})")
