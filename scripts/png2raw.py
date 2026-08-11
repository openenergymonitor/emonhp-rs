from PIL import Image

img = Image.open("emonhp_64x64.png").convert("1")

assert img.size == (64, 64)

data = bytearray()

for y in range(64):
    for x in range(0, 64, 8):
        byte = 0

        for i in range(8):
            # Pillow:
            # 0   = black
            # 255 = white
            #
            # embedded-graphics BinaryColor:
            # 0 = Off
            # 1 = On

            if img.getpixel((x + i, y)) == 0:
                byte |= 0x80 >> i

        data.append(byte)

with open("emonhp_64x64.raw", "wb") as f:
    f.write(data)

print(len(data))