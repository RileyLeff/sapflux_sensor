hey agent, we're in my hardware project for a sapflux sensor logic board. please read some of the
atopile files (a language for describing hardware schematics) without making any edits to understand
what we're doing here. the idea is to have a complete interface to a set of sap flux sensors -- we
support up to 48 thermistors in any topology, and up to a little under 5a @ 24v of heater output,
which should be about 8 sensors tops. there's a stm32 mcu, a "latch" system for detecting if the
heater is going off the rails and stopping it if it's at risk of starting a fire, onboard 32mb memory
 (no datalogger required), rs-485, a mux system for routing all of the thermistors through a single
adc with differential measurement, eeprom for storing config values, an ultra efficient buck
converter for powering the 3v3 side of things, and so on. this project supports 2 aims: 1.) i have
(what I believe to be) a better method of measuring sap flux than the status quo, i intend to test it
 on new hardware with this logic board. i'm a phd candidate in plant ecophysiology. i have some math
that shows if we measure the temperature change after a brief heat pulse (e.g. 1.5 - 2 sec instead of
 standard 3-8 sec, which gives us a sharper temperature response) very frequently, and instead of
treating the log ratio of temperature change upstream and downstream of the pulse as a fixed value,
we instead fit a curve to its derivative, we can measure directly things like probe misalignment and
thermal conductance to better isolate convection (sap flux). this has drastic improvements over the
current method in a simulated environment -- probe misalignment can cause like 70% error with 1mm
installation error, and thermal conduction varies meaningfully with variables scientists are
interested in measuring. think like water potential (water content), wood density, etc might all be
causally related to the variable a scientist is measuring (e.g. species responses, across a salinity
gradient, or so on) so when we say "we measured sap flux and it's higher over here", we're actually
saying "we measured temperature changes, made a bunch of assumptions, and we are simply going to
ignore the fact that our assumptions interfere with our interpretation".the derivative method i'm
proposing works because the upstream and downstream temperature probes are at different distances
from each other, a key difference from existing sensors. I have 2 pairs of identical analog sensors
ready to plug into this: 2 with equal probe spacing (classic) and 2 with offset (riley, me, style). i
 hope to get this manufactured so i can get this paper out. 2.) my second goal is to scale
measurement in the earth and environmental and ecological sciences. too long we've been held back by
expensive hardware that doesn't work very well. check out my rant at standardfield.org. the idea is
basically that with modern "lagging edge" tech, think cheap stuff from lcsc, and hobbyist-level tech
made en masse at jlcpcb, we can achieve 10-100x improvements in cost while maintaining or improving
measurement quality, and with better and more open standards, we can create an interoperable
ecosystem for acquiring and sharing data more efficiently. i'm already learning a lot with this
design. i think i came in with too heavy of a software mentality. what i've found is that software is
 free, but hardware is cheap, and those are extremely different things. software is also easy to
move, hardware is harder. so what i'm kinda leaning towards is that if something is cheap, throw it
on the board (e.g. $3 on nor flash prevents someone downstream from buying a shitty $2000 legacy
datalogger from campbell or whatever). other things are that we should optimize for the
deployment/site scale, not the individual sensor scale. this was originally a 1 board to 1 sensor
design, but frankly we don't need to duplicate these logic boards for each sensor. it's pretty common
 to have a whole plot that needs instrumenting, and the cable length can run far enough that you
still get some flexibility. we're basically optimizing for the guy installing 10,000 of these instead
 of the guy installing 1, even though it's still a huge improvement in accessibility to the guy
installing 1 just given the cost decrease overall. the other area for improvement in current hardware
 is power draw, this design basically draws nothing at idle. we do shift a lot of responsibility to
firmware, that's intentional, because it's free! Thoughts on this? let me know what you think and if
you're up to speed.