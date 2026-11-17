#!/usr/bin/env python3
"""Сравнение с оригиналом: наш рендер против настоящего браузера.

Цель браузера — «точь-в-точь как в оригинале», значит нужна измеримая метрика,
а не «похоже». Скрипт снимает эталон в Chrome, наш рендер через --dump и
считает: IoU чернил, корреляцию построчного и поколоночного профиля,
расхождение по горизонтальным полосам. Собирает композит: эталон | наш |
карта пропавшего (красный) и лишнего (синий).

Запуск:
    python3 scripts/visual-diff.py --url https://en.wikipedia.org/wiki/Web_browser
    python3 scripts/visual-diff.py --url ... --width 1600 --height 1000
"""

from __future__ import annotations

import argparse
import json
import subprocess
import tempfile
import time
from pathlib import Path
from typing import Dict, List, Optional

import numpy as np
from PIL import Image

DEFAULT_OURS = Path("browser/target/release/sepia-browser")
DEFAULT_CHROME = Path("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome")


def capture_ours(binary: Path, url: str, width: int, height: int, out: Path, timeout: int,
                 country: str = "") -> Dict:
    started = time.monotonic()
    extra = ["--country", country] if country else []
    result = subprocess.run(
        [str(binary), "--url", url, "--dump", str(out), "--width", str(width), "--height", str(height)] + extra,
        capture_output=True, text=True, timeout=timeout,
    )
    return {
        "ok": result.returncode == 0 and out.exists(),
        "ms": int((time.monotonic() - started) * 1000),
        "stdout": result.stdout,
        "stderr": result.stderr[-800:],
    }


def capture_reference(chrome: Path, url: str, width: int, height: int, out: Path, wait_ms: int) -> Dict:
    started = time.monotonic()
    with tempfile.TemporaryDirectory(prefix="sepia-ref-") as profile:
        command = [
            str(chrome),
            "--headless=new",
            "--disable-gpu",
            "--hide-scrollbars",
            "--no-first-run",
            "--no-default-browser-check",
            f"--user-data-dir={profile}",
            "--force-device-scale-factor=1",
            f"--window-size={width},{height}",
            f"--virtual-time-budget={wait_ms}",
            f"--screenshot={out}",
            url,
        ]
        result = subprocess.run(command, capture_output=True, text=True, timeout=wait_ms / 1000.0 + 60)
    return {
        "ok": result.returncode == 0 and out.exists(),
        "ms": int((time.monotonic() - started) * 1000),
        "stderr": result.stderr[-800:],
    }


def luminance(img: Image.Image) -> np.ndarray:
    a = np.asarray(img.convert("RGB"), dtype=np.float32)
    return 0.2126 * a[:, :, 0] + 0.7152 * a[:, :, 1] + 0.0722 * a[:, :, 2]


def ink_mask(img: Image.Image, threshold: float = 12.0) -> np.ndarray:
    lum = luminance(img)
    border = np.concatenate([lum[0:4].ravel(), lum[-4:].ravel(), lum[:, 0:4].ravel(), lum[:, -4:].ravel()])
    background = float(np.median(border))
    return np.abs(lum - background) > threshold


def profiles(mask: np.ndarray) -> tuple:
    rows = mask.sum(axis=1).astype(np.float64)
    cols = mask.sum(axis=0).astype(np.float64)
    return rows, cols


def correlation(a: np.ndarray, b: np.ndarray) -> float:
    if a.std() < 1e-6 or b.std() < 1e-6:
        return 1.0 if np.allclose(a, b) else 0.0
    return float(np.corrcoef(a, b)[0, 1])


def bands(mask_ref: np.ndarray, mask_ours: np.ndarray, count: int = 12) -> List[Dict]:
    """Где именно страницы расходятся: по горизонтальным полосам."""
    height = mask_ref.shape[0]
    step = max(1, height // count)
    result = []
    for index in range(count):
        y0, y1 = index * step, min((index + 1) * step, height)
        if y0 >= y1:
            break
        ref = float(mask_ref[y0:y1].mean())
        ours = float(mask_ours[y0:y1].mean())
        result.append({
            "band": index,
            "y": [y0, y1],
            "ref_ink": round(ref, 4),
            "ours_ink": round(ours, 4),
            "delta": round(ours - ref, 4),
        })
    return result


def best_shift(mask_ref: np.ndarray, mask_ours: np.ndarray, max_dy: int = 48,
               max_dx: int = 24) -> tuple:
    """Лучшее совмещение по сдвигу: сравнивать нужно раскладку, а не пиксели
    с точностью до шрифта и полей — иначе честная страница получает 20%."""
    def iou_at(dy: int, dx: int) -> float:
        moved = np.roll(np.roll(mask_ours, dy, axis=0), dx, axis=1)
        inter = float((mask_ref & moved).sum())
        union = float((mask_ref | moved).sum())
        return inter / union if union else 1.0

    best = (0, 0, 0.0)
    for dy in range(-max_dy, max_dy + 1, 4):
        for dx in range(-max_dx, max_dx + 1, 4):
            score = iou_at(dy, dx)
            if score > best[2]:
                best = (dy, dx, score)
    dy, dx = best[0], best[1]
    for fine_dy in range(dy - 3, dy + 4):
        for fine_dx in range(dx - 3, dx + 4):
            score = iou_at(fine_dy, fine_dx)
            if score > best[2]:
                best = (fine_dy, fine_dx, score)
    return best


def coarse_iou(mask_ref: np.ndarray, mask_ours: np.ndarray, cell: int = 8) -> float:
    """Сравнение раскладки, а не шрифтов: сетка 8x8 пикселей, пересечение
    «есть ли тут что-то» — терпит различия в метриках глифов и сглаживании."""
    height = (mask_ref.shape[0] // cell) * cell
    width = (mask_ref.shape[1] // cell) * cell
    if height == 0 or width == 0:
        return 0.0
    def blocks(mask: np.ndarray) -> np.ndarray:
        cropped = mask[:height, :width].reshape(height // cell, cell, width // cell, cell)
        return cropped.mean(axis=(1, 3)) > 0.02
    a, b = blocks(mask_ref), blocks(mask_ours)
    union = float((a | b).sum())
    return float((a & b).sum()) / union if union else 1.0


def composite(ref: Image.Image, ours: Image.Image, mask_ref: np.ndarray, mask_ours: np.ndarray, out: Path) -> None:
    size = (max(ref.width, ours.width), max(ref.height, ours.height))
    ref_p = Image.new("RGB", size, (255, 255, 255))
    ours_p = Image.new("RGB", size, (255, 255, 255))
    ref_p.paste(ref, (0, 0))
    ours_p.paste(ours, (0, 0))
    missing = mask_ref & ~mask_ours
    extra = mask_ours & ~mask_ref
    heat = np.full((size[1], size[0], 3), 255, dtype=np.uint8)
    heat[missing] = (220, 40, 40)
    heat[extra] = (40, 90, 220)
    heat_img = Image.fromarray(heat)
    canvas = Image.new("RGB", (size[0] * 3, size[1]), (255, 255, 255))
    canvas.paste(ref_p, (0, 0))
    canvas.paste(ours_p, (size[0], 0))
    canvas.paste(heat_img, (size[0] * 2, 0))
    canvas.save(out)


def compare(url: str, width: int, height: int, outdir: Path, ours_bin: Path, chrome: Path, wait_ms: int,
            country: str = "") -> Dict:
    site = url.split("//", 1)[-1].split("/", 1)[0].replace(".", "-")
    outdir.mkdir(parents=True, exist_ok=True)
    ours_path = outdir / "ours.png"
    ref_path = outdir / "reference.png"

    ours_info = capture_ours(ours_bin, url, width, height, ours_path, timeout=120, country=country)
    ref_info = capture_reference(chrome, url, width, height, ref_path, wait_ms)
    stats: Dict = {"url": url, "width": width, "height": height,
                   "ours": {"ok": ours_info["ok"], "ms": ours_info["ms"]},
                   "reference": {"ok": ref_info["ok"], "ms": ref_info["ms"]}}
    if not (ours_info["ok"] and ref_info["ok"]):
        stats["error"] = "one side produced no screenshot"
        stats["ours_stderr"] = ours_info["stderr"]
        stats["ref_stderr"] = ref_info["stderr"]
        return stats

    ref = Image.open(ref_path).convert("RGB")
    ours = Image.open(ours_path).convert("RGB")
    if ours.size != ref.size:
        ours = ours.resize(ref.size, Image.LANCZOS)
        stats["resized_ours"] = True

    m_ref = ink_mask(ref)
    m_ours = ink_mask(ours)
    dy, dx, iou = best_shift(m_ref, m_ours)
    m_aligned = np.roll(np.roll(m_ours, dy, axis=0), dx, axis=1)

    r_ref, c_ref = profiles(m_ref)
    r_ours, c_ours = profiles(m_aligned)
    band_rows = bands(m_ref, m_aligned)
    missing_bands = [b for b in band_rows if b["ref_ink"] > 0.02 and b["ours_ink"] < b["ref_ink"] * 0.4]
    extra_bands = [b for b in band_rows if b["ours_ink"] > 0.02 and b["ref_ink"] < b["ours_ink"] * 0.4]

    a = float(m_ref.mean())
    b = float(m_ours.mean())
    ink_similarity = 1.0 - abs(a - b) / max(a, b, 1e-6)
    band_gap = float(np.mean([abs(row["ours_ink"] - row["ref_ink"]) for row in band_rows])) if band_rows else 0.0
    band_similarity = max(0.0, 1.0 - band_gap / max(a, b, 1e-6))
    block_iou = coarse_iou(m_ref, m_aligned)
    score = 100.0 * (0.35 * block_iou + 0.20 * max(0.0, correlation(c_ref, c_ours))
                     + 0.20 * max(0.0, correlation(r_ref, r_ours))
                     + 0.10 * ink_similarity + 0.15 * band_similarity)

    stats.update({
        "score": round(score, 1),
        "iou": round(iou, 4),
        "block_iou": round(block_iou, 3),
        "ink_reference": round(a, 4),
        "ink_ours": round(b, 4),
        "row_correlation": round(correlation(r_ref, r_ours), 3),
        "column_correlation": round(correlation(c_ref, c_ours), 3),
        "bands": band_rows,
        "missing_bands": [b_["y"] for b_ in missing_bands],
        "extra_bands": [b_["y"] for b_ in extra_bands],
        "shift": [dy, dx],
    })
    shifted = Image.new("RGB", ours.size, (255, 255, 255))
    shifted.paste(ours, (dx, dy))
    composite(ref, shifted, m_ref, m_aligned, outdir / "composite.png")
    return stats


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--url", required=True)
    parser.add_argument("--width", type=int, default=1200)
    parser.add_argument("--height", type=int, default=900)
    parser.add_argument("--out", type=Path, default=Path("/tmp/sepia-visual"))
    parser.add_argument("--ours", type=Path, default=DEFAULT_OURS)
    parser.add_argument("--chrome", type=Path, default=DEFAULT_CHROME)
    parser.add_argument("--wait", type=int, default=8000, help="virtual time for the reference, ms")
    parser.add_argument("--country", default="", help="persona country for our render (RU, US, ...)")
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args()

    stats = compare(args.url, args.width, args.height, args.out, args.ours, args.chrome, args.wait,
                    country=args.country)
    (args.out / "stats.json").write_text(json.dumps(stats, ensure_ascii=False, indent=2))
    if args.json:
        print(json.dumps(stats, ensure_ascii=False, indent=2))
        return 0 if "error" not in stats else 1

    if "error" in stats:
        print(f"ошибка: {stats['error']}")
        print(f"  наш:      {stats.get('ours_stderr', '').strip().splitlines()[-1] if stats.get('ours_stderr') else '—'}")
        print(f"  эталон:   {stats.get('ref_stderr', '').strip().splitlines()[-1] if stats.get('ref_stderr') else '—'}")
        return 1
    print(f"{stats['url']}")
    print(f"  сходство       {stats['score']:>5.1f}/100")
    print(f"  раскладка      {stats['block_iou']:.2f} (блоки 8x8), пиксельный IoU {stats['iou']:.2f}, сдвиг {stats['shift']}")
    print(f"  чернила        эталон {stats['ink_reference'] * 100:.1f}%, наш {stats['ink_ours'] * 100:.1f}%")
    print(f"  построчно      r={stats['row_correlation']:+.2f}   поколоночно r={stats['column_correlation']:+.2f}")
    if stats["missing_bands"]:
        print(f"  пропало в полосах y={stats['missing_bands']}")
    if stats["extra_bands"]:
        print(f"  лишнее в полосах y={stats['extra_bands']}")
    print(f"  композит       {args.out / 'composite.png'}  (эталон | наш | пропало/лишнее)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
