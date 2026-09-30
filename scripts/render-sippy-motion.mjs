#!/usr/bin/env node
// Renders a short Sippy motion clip as SVG frames.
// Usage: node scripts/render-sippy-motion.mjs <frames-dir>
// Frames are rasterised with rsvg-convert and encoded with ffmpeg (see scripts/render-sippy-motion.sh).

import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const outDir = process.argv[2] ?? path.join(root, 'target/sippy-motion/frames');
fs.mkdirSync(outDir, { recursive: true });

const W = 1920, H = 1080, FPS = 60, DURATION = 8.5;
const BG = '#FFF6EF', INK = '#3A2A35', FONT = 'Caprasimo';

// --- asset loading ----------------------------------------------------------

function loadSippy(name) {
  const src = fs.readFileSync(path.join(root, 'assets/sippy', `${name}.svg`), 'utf8');
  return [...src.matchAll(/<path d="([^"]*)" fill="([^"]*)" transform="translate\(([^,]+),([^)]+)\)"/g)]
    .map(m => ({ d: m[1], fill: m[2], tx: +m[3], ty: +m[4] }));
}

// Part centres in the 512x512 source space, measured from the rendered parts.
const box = (w, h, x, y) => [x + w / 2, y + h / 2];
const CHARS = [
  { name: 'ready', label: 'ready', color: '#F95473', parts: loadSippy('ready'),
    eyes: [2, 3], centres: { 2: box(79, 51, 158, 186), 3: box(79, 52, 287, 220) } },
  { name: 'on-call', label: 'on a call', color: '#A7D933', parts: loadSippy('on-call'),
    eyes: [2, 3], ring: [4, 5],
    centres: { 2: box(78, 51, 157, 186), 3: box(78, 50, 290, 221), 4: box(30, 23, 358, 284), 5: box(24, 20, 357, 313) } },
  { name: 'dnd', label: 'do not disturb', color: '#A47AF9', parts: loadSippy('dnd'),
    moon: [2, 5, 6], centres: { 2: box(59, 69, 317, 147) } },
  { name: 'not-registered', label: 'not registered', color: '#F62E66', parts: loadSippy('not-registered'),
    cross: 2, sparks: [5, 6, 7],
    centres: { 2: box(47, 48, 335, 176), 5: box(26, 17, 395, 171), 6: box(17, 24, 382, 147), 7: box(23, 15, 393, 198) } },
];
const LOGO = { parts: loadSippy('logo'), eyes: [2, 3], centres: { 2: box(66, 37, 165, 202), 3: box(66, 37, 279, 202) } };

// Pivot at the bottom centre of the body so squash & stretch stays grounded.
const PIVOT_X = 256, PIVOT_Y = 455;

// --- easing -----------------------------------------------------------------

const clamp = (v, a = 0, b = 1) => Math.min(b, Math.max(a, v));
const lerp = (a, b, t) => a + (b - a) * t;
const prog = (t, start, dur) => clamp((t - start) / dur);
const easeInOut = t => t < 0.5 ? 4 * t * t * t : 1 - Math.pow(-2 * t + 2, 3) / 2;
const easeOut = t => 1 - Math.pow(1 - t, 3);
const easeIn = t => t * t * t;
const easeOutBack = t => { const c = 1.9; return 1 + (c + 1) * Math.pow(t - 1, 3) + c * Math.pow(t - 1, 2); };
// Damped wobble after an impact at `t0`; 0 before the impact.
const wobble = (t, t0, amp = 1, decay = 7, freq = 17) => t < t0 ? 0 : amp * Math.exp(-decay * (t - t0)) * Math.cos(freq * (t - t0));
// Smooth 0→1→0 window with ramps of `ramp` seconds.
const windowed = (t, start, end, ramp = 0.22) => easeInOut(prog(t, start, ramp)) * (1 - easeInOut(prog(t, end - ramp, ramp)));
// Quick eyelid close/open centred on `at`.
const blink = (t, at) => 1 - 0.9 * clamp(1 - Math.abs(t - at) / 0.07);

let seed = 7;
const rand = () => ((seed = (seed * 16807) % 2147483647) - 1) / 2147483646;

// --- timeline ----------------------------------------------------------------

const SLOT_X = i => W / 2 + (i - 1.5) * 390;
const FLOOR = 640;
const BASE_SCALE = 0.58;
const DROP_START = i => 0.15 + i * 0.16;
const DROP_DUR = 0.42;
const SPOT_START = 1.45, SPOT_LEN = 1.15;
const WAVE_START = SPOT_START + 4 * SPOT_LEN + 0.05; // 6.15
const MERGE_START = WAVE_START + 0.85;                // 7.0
const MERGE_DUR = 0.38;
const POP = MERGE_START + MERGE_DUR;                  // 7.38

const confetti = Array.from({ length: 26 }, (_, k) => ({
  angle: (k / 26) * Math.PI * 2 + rand() * 0.3,
  speed: 520 + rand() * 480,
  size: 7 + rand() * 9,
  color: CHARS[k % 4].color,
}));

// --- drawing -----------------------------------------------------------------

const fmt = n => +n.toFixed(2);

function partTransform(c, idx, fx) {
  const f = fx[idx];
  if (!f) return '';
  const [cx, cy] = c.centres[idx];
  const sx = f.sx ?? f.s ?? 1, sy = f.sy ?? f.s ?? 1;
  return ` transform="translate(${fmt(cx + (f.dx ?? 0))},${fmt(cy + (f.dy ?? 0))}) rotate(${fmt(f.r ?? 0)}) scale(${fmt(sx)},${fmt(sy)}) translate(${-cx},${-cy})"`;
}

function drawSippy(c, { x, y, scale, sx = 1, sy = 1, rot = 0, fx = {} }) {
  const body = c.parts.map((p, idx) => {
    const f = fx[idx];
    const op = f?.opacity ?? 1;
    if (op <= 0.001) return '';
    const inner = `<path d="${p.d}" fill="${p.fill}" transform="translate(${p.tx},${p.ty})"${op < 1 ? ` opacity="${fmt(op)}"` : ''}/>`;
    return f ? `<g${partTransform(c, idx, fx)}>${inner}</g>` : inner;
  }).join('');
  return `<g transform="translate(${fmt(x)},${fmt(y)}) rotate(${fmt(rot)}) scale(${fmt(scale * sx)},${fmt(scale * sy)}) translate(${-PIVOT_X},${-PIVOT_Y})">${body}</g>`;
}

function shadow(x, lift, scale, opacity = 1) {
  const k = clamp(1 - lift / 500, 0.35, 1);
  return `<ellipse cx="${fmt(x)}" cy="${FLOOR + 6}" rx="${fmt(140 * scale / BASE_SCALE * k)}" ry="${fmt(16 * scale / BASE_SCALE * k)}" fill="${INK}" opacity="${fmt(0.1 * k * opacity)}"/>`;
}

function frame(t) {
  const out = [];
  const back = [];
  const front = [];

  // Which mood is in the spotlight right now, and how strongly.
  const spot = CHARS.map((_, i) => windowed(t, SPOT_START + i * SPOT_LEN, SPOT_START + (i + 1) * SPOT_LEN, 0.2));
  const anySpot = Math.max(...spot);
  const merge = easeIn(prog(t, MERGE_START, MERGE_DUR));

  CHARS.forEach((c, i) => {
    let x = SLOT_X(i), lift = 0, scale = BASE_SCALE, sx = 1, sy = 1, rot = 0;
    const fx = {};
    const local = t - (SPOT_START + i * SPOT_LEN);
    const a = spot[i];

    // Drop in from above, then squash on landing.
    const d0 = DROP_START(i);
    const land = d0 + DROP_DUR;
    if (t < land) lift = lerp(900, 0, easeIn(prog(t, d0, DROP_DUR)));
    const q = wobble(t, land, 0.3);
    sx *= 1 + q; sy *= 1 - q;
    if (t < land) { sx *= 0.92; sy *= 1.1; } // stretched while falling

    // Idle breathing.
    const breathe = 0.018 * Math.sin(t * 2 * Math.PI * 0.9 + i * 1.3);
    sx *= 1 - breathe; sy *= 1 + breathe;

    // Spotlight: grow a little, the others step back.
    scale *= 1 + 0.26 * a - 0.08 * (anySpot - a);
    lift += 36 * a;

    if (c.name === 'ready' && local > 0 && local < SPOT_LEN) {
      // Two happy hops and a blink.
      const hop = Math.max(0, Math.sin(Math.PI * clamp((local - 0.15) / 0.34))) + Math.max(0, Math.sin(Math.PI * clamp((local - 0.52) / 0.3))) * 0.7;
      lift += 70 * hop * a;
      const hq = wobble(local, 0.49, 0.18, 9) + wobble(local, 0.82, 0.14, 9);
      sx *= 1 + hq; sy *= 1 - hq;
      const b = blink(local, 0.95);
      c.eyes.forEach(e => fx[e] = { sy: b });
    }

    if (c.name === 'on-call') {
      // Ring-ring: two bursts of shaking with pulsing sound dashes.
      const burst = local > 0 && local < SPOT_LEN
        ? windowed(local, 0.15, 0.45, 0.06) + windowed(local, 0.58, 0.88, 0.06) : 0;
      rot += 7 * burst * Math.sin(local * 2 * Math.PI * 16) * a;
      const pulse = 1 + 0.9 * burst * a * (0.5 + 0.5 * Math.sin(local * 2 * Math.PI * 8));
      c.ring.forEach((r, k) => fx[r] = { s: pulse, dx: 10 * burst * a * (k ? 1 : 0.6), dy: k ? 6 * burst * a : -4 * burst * a });
      const b = blink(t, 0.95);
      if (t < 1.2) c.eyes.forEach(e => fx[e] = { sy: b });
    }

    if (c.name === 'dnd') {
      // Sleepy sway, moon bobbing, z's drifting up.
      rot += 5 * Math.sin(local * 2.6) * a;
      lift += 10 * Math.sin(local * 3.2) * a;
      const moon = { r: 14 * Math.sin(local * 3) * a, s: 1 + 0.12 * a };
      c.moon.forEach(m => fx[m] = m === 2 ? moon : { opacity: 1 - 0.6 * a * (0.5 + 0.5 * Math.sin(local * 9 + m)) });
      c.centres[5] ??= c.centres[2];
      c.centres[6] ??= c.centres[2];
      fx[5] = { ...fx[5], r: moon.r };
      fx[6] = { ...fx[6], r: moon.r };
      for (let k = 0; k < 3; k++) {
        const zt = (local - 0.1 - k * 0.3) / 0.8;
        if (zt <= 0 || zt >= 1 || a <= 0) continue;
        const zx = x + 90 + k * 12 + 30 * zt + 8 * Math.sin(zt * 6);
        const zy = FLOOR - 300 - lift - 150 * zt;
        const op = Math.sin(Math.PI * zt) * a;
        front.push(`<text x="${fmt(zx)}" y="${fmt(zy)}" font-family="${FONT}" font-size="${fmt(34 + k * 12)}" fill="${c.color}" opacity="${fmt(op)}">z</text>`);
      }
    }

    if (c.name === 'not-registered' && local > 0) {
      // Head shake "nope", cross pops and sparks flicker.
      const shake = local < SPOT_LEN ? Math.sin(local * 2 * Math.PI * 5.5) * Math.exp(-2.2 * local) * windowed(local, 0.05, SPOT_LEN, 0.1) : 0;
      x += 26 * shake * a;
      rot += -5 * shake * a;
      const pop = local < SPOT_LEN ? 1 + 0.55 * a * Math.exp(-5 * clamp(local - 0.1, 0)) * Math.abs(Math.cos(local * 9)) : 1;
      fx[c.cross] = { s: pop, r: 45 * a * easeOut(clamp(local / 0.4)) };
      c.sparks.forEach((s, k) => fx[s] = { opacity: 1 - a * 0.9 * (Math.sin(local * 22 + k * 2) > 0 ? 1 : 0), s: 1 + 0.4 * a });
    }

    // Stadium wave.
    const w0 = WAVE_START + i * 0.09;
    const wp = prog(t, w0, 0.38);
    if (wp > 0 && wp < 1) { lift += 110 * Math.sin(Math.PI * wp); sx *= 0.94; sy *= 1.08; }
    const wq = wobble(t, w0 + 0.38, 0.22, 9);
    sx *= 1 + wq; sy *= 1 - wq;

    // Merge into the centre.
    if (merge > 0) {
      x = lerp(x, W / 2, merge);
      lift = lerp(lift, 120, merge);
      scale *= 1 - merge;
      rot += (i - 1.5) * 40 * merge;
    }
    if (scale <= 0.001) return;

    // Tinted halo behind the one in the spotlight.
    if (a > 0.01) {
      back.push(`<circle cx="${fmt(x)}" cy="${fmt(FLOOR - 200 - lift * 0.5)}" r="${fmt(170 + 110 * a)}" fill="${c.color}" opacity="${fmt(0.16 * a)}"/>`);
      const ly = FLOOR + 110 - 20 * a;
      front.push(`<text x="${SLOT_X(i)}" y="${fmt(ly)}" text-anchor="middle" font-family="${FONT}" font-size="54" fill="${c.color}" opacity="${fmt(a)}">${c.label}</text>`);
    }

    back.push(shadow(x, lift, scale, 1 - merge));
    out.push(drawSippy(c, { x, y: FLOOR - lift, scale, sx, sy, rot, fx }));
  });

  // Finale: logo pops out of the merge with confetti, wordmark rises.
  if (t >= POP) {
    const u = t - POP;
    const s = 0.78 * easeOutBack(clamp(u / 0.45));
    const q = wobble(t, POP + 0.3, 0.12, 6, 14);
    const b = blink(t, 8.05);
    const fx = { 2: { sy: b }, 3: { sy: b } };
    const logoY = FLOOR - 4;
    back.push(`<circle cx="${W / 2}" cy="${logoY - 170}" r="${fmt(250 * easeOut(clamp(u / 0.5)))}" fill="#F9587A" opacity="${fmt(0.08 * easeOut(clamp(u / 0.5)))}"/>`);
    confetti.forEach(p => {
      const ct = u;
      if (ct > 1.1) return;
      const cx = W / 2 + Math.cos(p.angle) * p.speed * ct;
      const cy = logoY - 180 + Math.sin(p.angle) * p.speed * ct + 700 * ct * ct;
      back.push(`<circle cx="${fmt(cx)}" cy="${fmt(cy)}" r="${fmt(p.size * (1 - ct / 1.1))}" fill="${p.color}"/>`);
    });
    back.push(shadow(W / 2, 0, s, clamp(u / 0.3)));
    out.push(drawSippy({ ...LOGO }, { x: W / 2, y: logoY, scale: Math.max(s, 0.001), sx: 1 + q, sy: 1 - q, fx }));

    const wt = easeOut(prog(t, POP + 0.25, 0.5));
    front.push(`<text x="${W / 2}" y="${fmt(850 + 40 * (1 - wt))}" text-anchor="middle" font-family="${FONT}" font-size="150" fill="${INK}" opacity="${fmt(wt)}">Sippy</text>`);
    const tt = easeOut(prog(t, POP + 0.5, 0.5));
    front.push(`<text x="${W / 2}" y="${fmt(918 + 20 * (1 - tt))}" text-anchor="middle" font-family="${FONT}" font-size="40" fill="${INK}" opacity="${fmt(0.55 * tt)}">SIP softphone for the Linux desktop</text>`);
  }

  // Soft fade in from and out to the background.
  const fade = Math.max(1 - clamp(t / 0.15), clamp((t - (DURATION - 0.2)) / 0.2));
  const veil = fade > 0 ? `<rect width="${W}" height="${H}" fill="${BG}" opacity="${fmt(fade)}"/>` : '';

  return `<svg xmlns="http://www.w3.org/2000/svg" width="${W}" height="${H}" viewBox="0 0 ${W} ${H}">`
    + `<rect width="${W}" height="${H}" fill="${BG}"/>${back.join('')}${out.join('')}${front.join('')}${veil}</svg>`;
}

const total = Math.round(DURATION * FPS);
for (let f = 0; f < total; f++) {
  fs.writeFileSync(path.join(outDir, `${String(f).padStart(4, '0')}.svg`), frame(f / FPS));
}
console.log(`${total} frames written to ${outDir}`);
