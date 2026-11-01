/* Lynx sprite test for mimesys: everything here is drawn by Suzy through cc65's TGI
   driver (sprites), plus a math check. */
#include <tgi.h>
#include <lynx.h>
#include <6502.h>
#include <stdlib.h>

#define POKE(a,v) (*(volatile unsigned char*)(a) = (v))
#define PEEK(a) (*(volatile unsigned char*)(a))

void main(void)
{
    long p;
    tgi_install(tgi_static_stddrv);
    tgi_init();
    CLI();
    while (tgi_busy()) ;
    tgi_setbgcolor(COLOR_BLACK);
    tgi_clear();
    tgi_setcolor(COLOR_RED);
    tgi_bar(10, 10, 49, 29);              /* filled box 40 x 20 */
    tgi_setcolor(COLOR_GREEN);
    tgi_line(0, 101, 159, 101);           /* bottom line */
    tgi_setcolor(COLOR_WHITE);
    tgi_outtextxy(60, 50, "LYNX");
    /* Suzy's multiplier: 1234 * 567 = 699678 */
    p = (long)1234 * 567;
    *(volatile long*)0x0080 = p;
    tgi_updatedisplay();
    for (;;) ;
}
