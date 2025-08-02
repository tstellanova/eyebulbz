
Drawing image data and primitves to one or more ST7789V displays,
using the Raspberry Pi Pico and Pico2 
( rp2040 and rp2350, repectively).


## development notes

-  We're using the Embassy embedded framework, which provides for multiple async tasks. 
-  We require two SPI peripherals in order to drive two separate displays.
