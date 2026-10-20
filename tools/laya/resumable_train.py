"""python resumable_train.py [--adamw8bit] <laya-train args>

`laya-train`, but an interrupted run picks up where it stopped. laya (pinned rev)
saves only the weights at each epoch end (`<out>/checkpoint_latest`), so this
swaps in a copy of `laya.train.train_model` that also writes `<out>/resume.pt`
(fp32 weights, AdamW and scheduler state, RNG, epochs done) after every epoch,
and starts from it when present. Run the same command again to resume.

A `checkpoint_latest` with an `epochs_done` file but no `resume.pt` (runs started
before this wrapper) resumes from the fp16 weights with fresh AdamW moments and
the LR schedule fast-forwarded to the same point.

The loop below is laya d74e8be's `train_model` with only the resume lines added;
re-copy it if LAYA_REV moves.
"""
import math
import os
import random
import sys

import torch

from laya import train as T

RESUME = None  # <out>, set from argv


def train_model(model, tok, items, config, device, max_len, head_max_len, on_epoch_end=None, parallel=False):
    config.validate()
    if not items:
        raise ValueError("no training items")
    amp = (device.type == "cuda") if config.amp is None else bool(config.amp)
    checkpointing = amp if config.gradient_checkpointing is None else bool(config.gradient_checkpointing)

    if config.freeze_encoder:
        for p in model.encoder.parameters():
            p.requires_grad_(False)
    elif checkpointing and hasattr(model.encoder, "gradient_checkpointing_enable"):
        model.encoder.gradient_checkpointing_enable(gradient_checkpointing_kwargs={"use_reentrant": False})
    model.head_checkpointing = checkpointing
    model.to(device).train()
    if config.freeze_encoder:
        model.encoder.eval()

    groups = [{"params": [p for n, p in model.named_parameters()
                          if not n.startswith("encoder.") and p.requires_grad], "lr": config.head_lr}]
    if not config.freeze_encoder:
        groups.insert(0, {"params": [p for n, p in model.named_parameters()
                                     if n.startswith("encoder.") and p.requires_grad], "lr": config.encoder_lr})
    optimizer = torch.optim.AdamW(groups, weight_decay=config.weight_decay)
    steps_per_epoch = math.ceil(len(items) / config.micro_batch)
    scheduler = torch.optim.lr_scheduler.CosineAnnealingLR(
        optimizer,
        T_max=T.optimizer_updates(len(items), config.micro_batch, config.grad_accum, config.epochs),
        eta_min=config.min_lr)
    scaler = torch.amp.GradScaler("cuda") if amp and device.type == "cuda" else None

    torch.manual_seed(config.seed)
    order_rng = random.Random(config.seed)
    params = [p for g in groups for p in g["params"]]
    history = []
    first = resume(model, optimizer, scheduler, order_rng, len(items), config, device)
    for epoch in range(first, config.epochs):
        epoch_items = list(items)
        random.Random(config.seed + epoch).shuffle(epoch_items)
        sigma = T.sigma_at(epoch, config.epochs, config.sigma_start, config.sigma_end)
        total, n_steps = 0.0, 0
        optimizer.zero_grad(set_to_none=True)
        for start in range(0, len(epoch_items), config.micro_batch):
            chunk = [T.encode_item(tok, it, max_len, head_max_len,
                                   T.draw_option_order(it, order_rng, config.shuffle_options), parallel)
                     for it in epoch_items[start:start + config.micro_batch]]
            batch = T.collate_items([chunk], tok.pad_token_id)
            logits = T._forward(model, batch, device, amp, config.freeze_encoder)
            mask = batch["marker_mask"].to(device)
            target = batch["target"].to(device)
            if config.loss == "rlcd":
                loss = T.rlcd_loss(logits, target, mask, batch["qtype"].to(device), sigma,
                                   config.rl_samples, config.w_sph, config.w_rps)
            else:
                loss = T.soft_ce_loss(logits, target, mask)
            window_start = (n_steps // config.grad_accum) * config.grad_accum
            window_size = min(config.grad_accum, steps_per_epoch - window_start)
            scaled = loss / window_size
            if scaler is not None:
                scaler.scale(scaled).backward()
            else:
                scaled.backward()
            n_steps += 1
            if n_steps % config.grad_accum == 0 or start + config.micro_batch >= len(epoch_items):
                if scaler is not None:
                    scaler.unscale_(optimizer)
                torch.nn.utils.clip_grad_norm_(params, config.grad_clip)
                if scaler is not None:
                    scaler.step(optimizer)
                    scaler.update()
                else:
                    optimizer.step()
                scheduler.step()
                optimizer.zero_grad(set_to_none=True)
            total += loss.item()
            if config.log_every and n_steps % config.log_every == 0:
                print("epoch %d/%d step %d loss %.4f" % (epoch + 1, config.epochs, n_steps, loss.item()), flush=True)
        mean = total / max(1, n_steps)
        history.append(mean)
        print("epoch %d/%d mean loss %.4f" % (epoch + 1, config.epochs, mean), flush=True)
        if on_epoch_end is not None:
            on_epoch_end(epoch, mean)
        save(epoch + 1, model, optimizer, scheduler, order_rng)
    model.eval()
    if os.path.exists(state_path()):
        os.remove(state_path())  # 3-4 GB; the finished checkpoint replaces it
    return history


def state_path():
    return os.path.join(RESUME, "resume.pt")


def save(done, model, optimizer, scheduler, order_rng):
    tmp = state_path() + ".tmp"
    torch.save({"epochs_done": done, "model": model.state_dict(), "optimizer": optimizer.state_dict(),
                "scheduler": scheduler.state_dict(), "torch_rng": torch.get_rng_state(),
                "order_rng": order_rng.getstate()}, tmp)
    os.replace(tmp, state_path())
    with open(os.path.join(RESUME, "checkpoint_latest", "epochs_done"), "w") as f:
        f.write(str(done))


def resume(model, optimizer, scheduler, order_rng, n_items, config, device):
    """Restore the last finished epoch; returns the epoch to start at (0 = fresh run)."""
    if os.path.exists(state_path()):
        s = torch.load(state_path(), map_location=device, weights_only=False)
        model.load_state_dict(s["model"])
        optimizer.load_state_dict(s["optimizer"])
        scheduler.load_state_dict(s["scheduler"])
        torch.set_rng_state(s["torch_rng"])
        order_rng.setstate(s["order_rng"])
        print("resumed from %s after epoch %d" % (state_path(), s["epochs_done"]), flush=True)
        return s["epochs_done"]
    latest = os.path.join(RESUME, "checkpoint_latest")
    marker = os.path.join(latest, "epochs_done")
    if not os.path.exists(marker):
        return 0
    with open(marker) as f:
        done = int(f.read())
    from safetensors.torch import load_file
    model.load_state_dict({k: v.float() for k, v in load_file(os.path.join(latest, "model.safetensors")).items()})
    per_epoch = T.optimizer_updates(n_items, config.micro_batch, config.grad_accum, 1)
    for _ in range(done * per_epoch):
        scheduler.step()  # warns that optimizer.step() never ran; harmless here
    torch.manual_seed(config.seed + done)
    print("resumed weights-only from %s after epoch %d (fresh AdamW moments, LR schedule at update %d)"
          % (latest, done, done * per_epoch), flush=True)
    return done


def main(argv):
    global RESUME
    if argv[:1] == ["--adamw8bit"]:  # 6 GB GPUs
        import bitsandbytes
        torch.optim.AdamW = bitsandbytes.optim.AdamW8bit
        argv = argv[1:]
    RESUME = argv[argv.index("--out") + 1]
    os.makedirs(RESUME, exist_ok=True)
    T.train_model = train_model
    from laya.train_cli import main as laya_train
    return laya_train(argv)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
