

Drawing SVG paths onto dual ST7789 displays, using the Raspberry Pi Pico-2 
(which uses the rp2350 microcontroller). 

## Development notes

-  We're using the Embassy embedded framework, which provides for multiple async tasks. 
-  We require two SPI peripherals in order to drive two separate displays.
-  rp2040 support was dropped in favor of the rp2350 to support two (eye) framebuffers.
-  Rendering is split between both cores of the rp2350, one core per eye (roughly).

