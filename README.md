# Sap Flux Logic Board

![Sap Flux Logic Board v0.1](assets/sap_flux_logic_board_v0.1.jpg)

An open hardware logic board for experimental heat-pulse sap flux sensors.

Sap flux sensors estimate how water moves through trees by watching heat move
through sapwood. Current sensors are expensive, power-hungry, and not all that accurate.

This repo contains some hardware I'm developing to fix all of that stuff!

## Current Status

The board is real and in-hand. The first revision literally exploded on power-on because I did not know the convention for communicating diode orientation to JLCPCB.

The repo now contains a second revision of the board with some minor improvements to the power layout, and I did not bork the layout confirmation on the second order. The board is working great in the lab!

So far, the beauty and splendor promised in the simulations seems to transfer to real-world situations. I've seen enough to make me confident that:

1. The new measurement theory works, and crushes the old theory. I'm presenting on this at AGU in December 2026!
2. The logic board design works. I don't think it's fully optimal/ready-to-scale yet, but my initial lab testing has convinced me it's sound.

I'll link to my data and AGU presentation when I have them ready for public consumption. In the meantime, you're welcome to check out the hardware here in this repo.

I haven't yet fully deployed in real field conditions, and I imagine there will be some empirical caveats and gotchas to navigate in those deployments. But so far so good!

The main thing to figure out from here is basically: now that we can actually inference the sap flux model accurately, are there variable, empirical features of the measurement that we can solve with better theory or engineering? Empiricism on plants is hard: they're complicated and diverse. The less empiricism we have to do on our estimates, the higher our bandwidth gets.

## What's Going On In Here?

Want to measure how fast water moves through a tree at high frequency? It's useful to know because it connects a tree to nearly all of its environmental conditions. We have a model to measure this and a sensor to provide data for it, but our model is broken in two specific ways:

- The actual probes in the tree get slightly misaligned when you install them, and tiny misalignments produce mega-huge errors.
- The probes work by introducing a heat pulse into the tree and measuring how fast it moves. How much of that movement is attributable to convection (sap flux) versus conduction? Current implementations attribute all variation to convection, which is a bad assumption.

The first problem is massive in magnitude; the second one is smaller (convection does genuinely change more than conduction) but more destructive per unit error because it's deeply confounded with stuff on the tree that you actually want to measure, like water potential or wood density.

This is open hardware that allows me to test my newly derived method for inferring sap flux rates from the raw data: we can't use commercial sensors because they're locked down and they discard most of the raw signal in favor of restrictive summary statistics. I do have a separate project that decompresses those summary statistics into distributions of possible raw measurements, but it's better to just keep the data in the first place.

Since we're already in the business of building custom hardware, we can get the power consumption down and the costs down as well.

## What The Board Does

Here are some design notes:

- 48 thermistor channels through three analog multiplexers.
- One ADS1115 ADC for heat-pulse temperature traces and excitation sensing.
- Support for analog sensor heads with internal 10k half-bridge thermistor setups.
- Shorter, higher-power heat pulses (which flatten out the signal less).
- Up to 120W power output in very short bursts across 8 sensors with ~40-45 ohm heater resistance.
- 24V field power input with protection and a 3.3V buck supply.
- RS-485.
- 32 MB SPI flash.
- I2C EEPROM.
- STM32G0B1 microcontroller target, intended for embedded Rust firmware with
  the [Embassy](https://github.com/embassy-rs/embassy) framework.
- Six layers, KiCad layout, fabbed at JLC.

I put a fairly complex analog mechanism for shutting down the heater on here because I'm worried about the case where a broken sensor in a dead tree starts a forest fire. This is probably the area that could use the most simplification in future revisions. Right now it mostly serves to annoyingly latch my heater shut by accident if the power supply ever blips in a way that the power shutdown doesn't like. I've made some firmware improvements for handling it, but I think the hardware could be improved there.

## Compared With Current Workflows

This one:
- Open, hackable
- <1mW idle power consumption (if you're creative with shutting off the RS-485 transceiver, e.g., a 15-second polling interval)
- Maybe $50/board at small/medium volumes, $20/board at large volumes

Commercial:
- Closed
- >100mW idle power consumption
- $1000/sensor + $1000+ logger
- More expensive infrastructure to support the greater power consumption

I don't think this is the final sensor topology, but 8x analog sensor heads per board is really nice for testing the method in the lab. I figure I'll make a more integrated, more deployment-friendly design in the future.

## Reproducibility

Atopile is the canonical electrical design source for this board:

- [`elec/main.ato`](elec/main.ato)
- [`elec/ato.yaml`](elec/ato.yaml)
- [`elec/`](elec/)

Note that I found it *really nice* to work with Claude via atopile! I'm still doing the layout by hand, but putting the electrical connectivity in a Python-like language made iteration and review a breeze.

Same with the PCBParts MCP server. I always found navigating parts to be a big barrier between me, the inexperienced hardware fan that fools around with breadboards sometimes, and building a PCB from scratch. Claude is really good at that stuff if you provide good tools.

The KiCad layout is here:

- [`elec/layouts/default/default.kicad_pcb`](elec/layouts/default/default.kicad_pcb)

The shipped fabrication packages are frozen here:

- v0.2 (current, working board): [`releases/sap-flux-adapter-v0.2-2026-06-26/`](releases/sap-flux-adapter-v0.2-2026-06-26/)
- v0.1 (first board; the folder name says v1.1, but the board itself is labeled v0.1): [`releases/sap-flux-adapter-v1.1-shipped-2026-03-21/`](releases/sap-flux-adapter-v1.1-shipped-2026-03-21/)

For more detail, see [`docs/reproducibility.md`](docs/reproducibility.md). Note that it's a Claude-written document; I suggest pointing your LLM agent at the repo to inspect the guts of it. This README is written by hand (by Riley) but the rest of the repo is a Claude party.

## Internal Links (Claude's Docs)

- Specs: [`docs/specs.md`](docs/specs.md)
- Reproducibility notes: [`docs/reproducibility.md`](docs/reproducibility.md)
- Design history: [`docs/design-history/`](docs/design-history/)
- Shipped releases: [v0.2](releases/sap-flux-adapter-v0.2-2026-06-26/), [v0.1](releases/sap-flux-adapter-v1.1-shipped-2026-03-21/)
