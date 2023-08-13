s = ''.join(x for x in '4c540900305700 0201 03 6cfd' if x != ' ')
out = ''
for x in range(0, len(s), 2):
    out += '0x' + s[x] + s[x + 1] + ', '

print(out)
