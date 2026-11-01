// ttk.js
// Time to kill: how long the whole boss fight should last, from the boss's
// remaining HP and the rate the party is taking it off.
//
// The rate comes from the boss's HP, not the rows' DPS, so healing, shields
// and damage the meter does not attribute all count. It is the HP lost over
// the last WINDOW_MS (the whole fight until that much has passed), smoothed so
// a burst window does not cut the estimate short: a faster rate is taken in
// over RISE_TAU_MS, a slower one over FALL_TAU_MS.
const createTtkEstimator = ({
  windowMs = 20000,
  riseTauMs = 30000,
  fallTauMs = 10000,
  minElapsedMs = 5000,
  maxTotalMs = 99 * 60000,
} = {}) => {
  let targetId = null;
  let samples = [];
  let rate = null;
  let lastT = null;

  const reset = (id = null) => {
    targetId = id;
    samples = [];
    rate = null;
    lastT = null;
  };

  // id: the boss; t: fight time (ms); remaining, max: its HP.
  // Returns the predicted length of the whole fight (ms), or null.
  const update = (id, t, remaining, max) => {
    const fightMs = Number(t);
    const hp = Number(remaining);
    const maxHp = Number(max);
    if (!(maxHp > 0) || !(hp >= 0) || !Number.isFinite(fightMs)) return null;
    // Another boss, or the timer went back: a new fight.
    if (id !== targetId || (lastT !== null && fightMs < lastT)) reset(id);
    if (hp <= 0) return null;

    const last = samples[samples.length - 1];
    // HP went up (a heal, a phase): measure from here.
    if (last && hp > last.hp) samples = [];
    if (last && samples.length && fightMs === last.t) last.hp = hp;
    else samples.push({ t: fightMs, hp });
    // Keep one sample at or before the window's start.
    while (samples.length > 2 && samples[1].t <= fightMs - windowMs) samples.shift();

    if (fightMs < minElapsedMs) {
      lastT = fightMs;
      return null;
    }
    const first = samples[0];
    const span = fightMs - first.t;
    const fightAverage = (maxHp - hp) / fightMs;
    const observed = span >= windowMs / 2 ? (first.hp - hp) / span : fightAverage;

    if (rate === null) {
      const seed = fightAverage > 0 ? fightAverage : observed;
      if (seed > 0) rate = seed;
    } else if (observed >= 0 && lastT !== null && fightMs > lastT) {
      const tau = observed > rate ? riseTauMs : fallTauMs;
      rate += (1 - Math.exp(-(fightMs - lastT) / tau)) * (observed - rate);
    }
    lastT = fightMs;
    if (!(rate > 0)) return null;
    const total = fightMs + hp / rate;
    return total <= maxTotalMs ? total : null;
  };

  return { update, reset };
};

if (typeof module !== "undefined") module.exports = { createTtkEstimator };
