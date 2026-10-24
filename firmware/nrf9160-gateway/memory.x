/*
 * CircuitDojo's 2021 MCUboot layout puts a 0x200-byte image header at
 * 0x0001_0000. TF-M occupies 0x0001_0200..0x0001_C200 and launches this
 * application in non-secure mode at 0x0001_C200.
 *
 * TF-M reserves 0x2000_0000..0x2000_7fff for secure RAM. Nordic libmodem's
 * shared control/TX/RX area then occupies 0x2000_8000..0x2000_c4e7, matching
 * the partition layout produced by NCS 2.1.2 for this Feather.
 */
MEMORY
{
  FLASH : ORIGIN = 0x0001C200, LENGTH = 0x00063E00
  RAM   : ORIGIN = 0x2000C4E8, LENGTH = 0x00033B18
}

/* nRF9160 has 81 vector entries (0x144 bytes); align following code to 8. */
_stext = ORIGIN(FLASH) + 0x148;
