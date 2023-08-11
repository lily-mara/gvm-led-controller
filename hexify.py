s = '4c54090030570000010032fe'
out = ''
for x in range(0, len(s), 2):
    out += '0x' + s[x] + s[x + 1] + ', '

print(out)
