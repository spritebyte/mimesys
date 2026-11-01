#define POKE(a,v) (*(volatile unsigned char*)(a) = (v))
#define PEEK(a) (*(volatile unsigned char*)(a))
void main(void)
{
    unsigned int i;
    POKE(0xFDA1, 0x0F);            /* colour 1: green 15 */
    POKE(0xFDB1, 0x0F);            /* red 15: yellow */
    POKE(0xFDA2, 0x00);            /* colour 2: blue */
    POKE(0xFDB2, 0xF0);
    POKE(0xFD94, 0x00);            /* screen at A000 */
    POKE(0xFD95, 0xA0);
    for (i = 0; i < 80; ++i) POKE(0xA000 + i, 0x11);
    for (i = 0; i < 80; ++i) POKE(0xA000 + 80 * 101 + i, 0x22);
    for (;;) {
        /* echo the joypad into the middle line */
        unsigned char j = PEEK(0xFCB0);
        POKE(0xA000 + 80 * 50, j);
    }
}
