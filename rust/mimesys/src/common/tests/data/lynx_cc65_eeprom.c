/* Lynx EEPROM test for mimesys: cc65's 93C46 routines (16-bit words).
   (cc65's older lynx_eeprom_read returns the high byte in Y instead of X, so the
   newer _93c46 entry points are used.) */
#include <lynx.h>

unsigned __fastcall__ lynx_eeread_93c46(unsigned cell);
unsigned __fastcall__ lynx_eewrite_93c46(unsigned cell, unsigned val);

#define POKEW(a,v) (*(volatile unsigned*)(a) = (v))

void main(void)
{
    unsigned v;
    v = lynx_eeread_93c46(5);         /* preloaded by the test */
    POKEW(0x82, v);
    lynx_eewrite_93c46(3, 0x1234);
    v = lynx_eeread_93c46(3);
    POKEW(0x80, v);
    POKEW(0x84, 0xBEEF);              /* done */
    for (;;) ;
}
