; SNES stage-1 test cartridge for mimesys (LoROM, 32 KB). Built with ca65 / ld65.
; Mode 1: BG1 shows tile 1 (solid colour 1, red) at map position (5, 5); sprite 0 is
; tile 2 (solid colour 1 of sprite palette 0, green) at (100, 50). HDMA writes the
; backdrop colour per line: blue on lines 1-100, then black. NMI counts frames into
; $0000 and copies the auto-read joypad into $0002. Waits for the APU's AA BB first.
.p816
.smart

.segment "CODE"
reset:
    sei
    clc
    xce                     ; native mode
    rep #$38                ; 16-bit A/X/Y, binary
    ldx #$1FFF
    txs
    lda #$0000
    tcd
    sep #$20                ; 8-bit A
    lda #$80
    sta $2100               ; forced blank
    ; wait for the APU boot handshake
:   lda $2140
    cmp #$AA
    bne :-
    lda $2141
    cmp #$BB
    bne :-
    ; Mode 1, BG1 map at VRAM word $1000 (32x32), chars at $0000
    lda #$01
    sta $2105
    lda #$10
    sta $2107
    stz $210B
    ; sprites: 8x8/16x16, name base 0 (word $0000)
    stz $2101
    ; tiles 1 and 2 (4bpp, 16 words each): plane 0 all ones -> colour 1
    lda #$80
    sta $2115               ; increment after the high byte
    ldx #$0010              ; tile 1 at word $10
    stx $2116
    ldy #$0000
:   lda #$FF                ; planes 0,1 row: low = plane 0, high = plane 1
    sta $2118
    stz $2119
    iny
    cpy #8
    bne :-
    ldy #$0000
:   stz $2118               ; planes 2,3
    stz $2119
    iny
    cpy #8
    bne :-
    ; tile 2 the same (word $20), by DMA from ROM this time
    ldx #$0020
    stx $2116
    lda #$01                ; mode 1: 2118 / 2119
    sta $4300
    lda #$18
    sta $4301
    ldx #.loword(tile_data)
    stx $4302
    lda #^tile_data
    sta $4304
    ldx #32
    stx $4305
    lda #$01
    sta $420B
    ; clear the BG1 map (32x32 words at $1000), then put tile 1 at (5, 5)
    ldx #$1000
    stx $2116
    ldx #$0000
:   stz $2118
    stz $2119
    inx
    cpx #$0400
    bne :-
    ldx #($1000 + 5 * 32 + 5)
    stx $2116
    lda #$01
    sta $2118
    stz $2119
    ; palette: colour 1 red, sprite colour 129 green
    lda #$01
    sta $2121
    lda #$1F
    sta $2122
    stz $2122
    lda #129
    sta $2121
    lda #$E0
    sta $2122
    lda #$03
    sta $2122
    ; OAM: sprite 0 at (100, 50), tile 2, priority 3; the rest off screen (y = 240)
    stz $2102
    stz $2103
    lda #100
    sta $2104
    lda #50
    sta $2104
    lda #2
    sta $2104
    lda #$30
    sta $2104
    ldx #1
:   stz $2104               ; x
    lda #240
    sta $2104               ; y
    stz $2104
    stz $2104
    inx
    cpx #128
    bne :-
    ldx #0
:   stz $2104               ; high table: small, x < 256
    inx
    cpx #32
    bne :-
    ; HDMA channel 1: backdrop colour (CGRAM 0) via 2121 / 2122 (mode 2: 2122 twice)
    lda #$03                ; mode 3: 2121, 2121, 2122, 2122
    sta $4310
    lda #$21
    sta $4311
    ldx #.loword(hdma_table)
    stx $4312
    lda #^hdma_table
    sta $4314
    lda #$02
    sta $420C
    ; main screen: BG1 + OBJ, display on
    lda #$11
    sta $212C
    lda #$0F
    sta $2100
    ; NMI + auto joypad
    stz $00
    stz $01
    lda #$81
    sta $4200
    cli
forever:
    wai
    bra forever

nmi:
    rep #$20
    pha
    sep #$20
    lda $4210               ; acknowledge
    inc $00
    bne :+
    inc $01
:
    ; wait for the auto read, then copy JOY1
:   lda $4212
    and #$01
    bne :-
    rep #$20
    lda $4218
    sta $02
    pla
    rti

irq:
    rti

.segment "RODATA"
tile_data:
    .repeat 8
    .byte $FF, $00
    .endrepeat
    .repeat 8
    .byte $00, $00
    .endrepeat
hdma_table:
    ; 100 lines: CGRAM address 0, colour blue ($7C00)
    .byte 100
    .byte $00, $00, $00, $7C
    ; then 1 line: black, held for the rest of the frame
    .byte 1
    .byte $00, $00, $00, $00
    .byte 0

.segment "HEADER"
    .byte "MIMESYS SNES TEST    "   ; 21 bytes
    .byte $20                       ; LoROM, slow
    .byte $00                       ; ROM only
    .byte $08                       ; 256 KB (size code; file is 32 KB)
    .byte $00                       ; no SRAM
    .byte $01                       ; region
    .byte $00                       ; developer
    .byte $00                       ; version
    .word $0000                     ; complement (not checked)
    .word $FFFF                     ; checksum

.segment "VECTORS"
    ; native (FFE0): unused, unused, COP, BRK, ABORT, NMI, unused, IRQ
    .word 0, 0, 0, 0, 0, nmi, 0, irq
    ; emulation (FFF0): unused, unused, COP, unused, ABORT, NMI, RESET, IRQ/BRK
    .word 0, 0, 0, 0, 0, nmi, reset, irq
