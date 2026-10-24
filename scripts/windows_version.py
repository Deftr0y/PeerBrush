"""Bounded standard Windows version metadata; byte inspection is not signature trust."""
import re
import struct

from pe_signing import layout

TEMPLATE = 'packaging/windows/version.rc.in'
RESOURCE_LIMIT = 65536


def strings(version):
    return dict(CompanyName='PeerBrush', FileDescription='PeerBrush graphics editor', FileVersion=version,
        InternalName='peerbrush', OriginalFilename='peerbrush.exe', ProductName='PeerBrush',
        ProductVersion=version, Comments='GPL-3.0-only')


def numeric(version):
    if (type(version) is not str or len(version) > 128
            or not re.fullmatch(r'(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?', version)):
        raise ValueError('Windows resource requires the exact bounded Cargo version.')
    value = [int(part) for part in re.split('[-+]', version, maxsplit=1)[0].split('.')] + [0]
    if any(part > 65535 for part in value):
        raise ValueError('Windows fixed version components must fit u16.')
    return value


def required(sources):
    names = sources if isinstance(sources, dict) else {name for name, _, _ in sources}
    return TEMPLATE in names


def version_bytes(data):
    """Select one RT_VERSION/ID1/English leaf through validated PE32+ sections."""
    checksum, _, _, _, _ = layout(data)
    optional = checksum - 64
    count = struct.unpack_from('<H', data, optional - 18)[0]
    table = optional + struct.unpack_from('<H', data, optional - 4)[0]
    sections = []
    for index in range(count):
        virtual_size, address, size, pointer = struct.unpack_from('<IIII', data, table + 40 * index + 8)
        extent = max(virtual_size, size)
        if extent:
            if not address or address + extent > 2 ** 32:
                raise ValueError('Invalid PE virtual section range.')
            sections.append((address, address + extent, size, pointer))
    sections.sort()
    if any(left[1] > right[0] for left, right in zip(sections, sections[1:])):
        raise ValueError('Overlapping PE virtual section ranges.')

    def mapped(address, size):
        selected = [row for row in sections if row[0] <= address and address + size <= row[0] + row[2]]
        if not size or len(selected) != 1:
            raise ValueError('Resource RVA does not select one complete raw section range.')
        row = selected[0]
        start = row[3] + address - row[0]
        return data[start:start + size]

    address, size = struct.unpack_from('<II', data, optional + 112 + 2 * 8)
    if not address or not 16 <= size <= 1024 * 1024:
        raise ValueError('Missing or excessive PE resource directory.')
    resource = mapped(address, size)
    visited, regions = set(), []

    def region(start, size):
        if (start % 4 or start < 0 or size <= 0 or start > len(resource) - size
                or any(start < end and begin < start + size for begin, end in regions)):
            raise ValueError('Resource directory/data-entry bounds overlap or are invalid.')
        regions.append((start, start + size))

    def directory(offset):
        if offset in visited or offset > len(resource) - 16:
            raise ValueError('Resource directory is cyclic or truncated.')
        visited.add(offset)
        named, identifiers = struct.unpack_from('<HH', resource, offset + 12)
        count = named + identifiers
        if not 1 <= count <= 256:
            raise ValueError('Resource directory entry count exceeds its bound.')
        region(offset, 16 + count * 8)
        result, previous_name, previous_id = {}, None, None
        for index in range(count):
            key, value = struct.unpack_from('<II', resource, offset + 16 + index * 8)
            if bool(key & 0x80000000) != (index < named):
                raise ValueError('Resource directory names/IDs are inconsistent.')
            if key & 0x80000000:
                start = key & 0x7fffffff
                if start % 2 or start > len(resource) - 2:
                    raise ValueError('Invalid resource name bounds.')
                length = struct.unpack_from('<H', resource, start)[0]
                if not 0 < length <= 128 or start + 2 + 2 * length > len(resource):
                    raise ValueError('Invalid bounded resource name.')
                try:
                    encoded = resource[start + 2:start + 2 + 2 * length]
                    key = encoded.decode('utf-16le')
                except UnicodeError:
                    raise ValueError('Invalid resource name UTF-16.') from None
                order = struct.unpack('<' + str(length) + 'H', encoded)
                if previous_name is not None and order <= previous_name:
                    raise ValueError('Resource names must be strictly ordered by case-sensitive UTF-16 units.')
                previous_name = order
            else:
                if key > 65535 or (previous_id is not None and key <= previous_id):
                    raise ValueError('Resource IDs must be valid and strictly ascending for native lookup.')
                previous_id = key
            if key in result:
                raise ValueError('Duplicate resource name or ID.')
            result[key] = value
        return result

    def child(value):
        if not value & 0x80000000:
            raise ValueError('Expected a version resource subdirectory.')
        return directory(value & 0x7fffffff)

    root = directory(0)
    if 16 not in root:
        raise ValueError('Missing standard RT_VERSION resource.')
    versions = child(root[16])
    if set(versions) != {1}:
        raise ValueError('Version resources must have exactly ID1.')
    languages = child(versions[1])
    if set(languages) != {0x409} or languages[0x409] & 0x80000000:
        raise ValueError('Version resource requires exactly the reviewed English language.')
    offset = languages[0x409]
    region(offset, 16)
    address, size, page, reserved = struct.unpack_from('<IIII', resource, offset)
    if not 0 < size <= RESOURCE_LIMIT or page not in [0, 1200] or reserved:
        raise ValueError('Invalid version resource data entry.')
    result = mapped(address, size)
    # The selected value must live in this resource directory, after its tables.
    relative = address - struct.unpack_from('<I', data, optional + 112 + 2 * 8)[0]
    if (relative < 0 or relative + size > len(resource)
            or any(relative < end and begin < relative + size for begin, end in regions)):
        raise ValueError('Version value overlaps its resource tables or leaves the directory.')
    return result


def inspect(data, version):
    expected = numeric(version)
    resource = version_bytes(data)
    nodes = 0

    def align(value):
        return (value + 3) & ~3

    def zeros(begin, end):
        if end > len(resource) or any(resource[begin:end]):
            raise ValueError('Version resource contains invalid alignment padding.')

    def block(start, limit, depth=0):
        nonlocal nodes
        nodes += 1
        if depth > 4 or nodes > 32 or start % 4 or start > limit - 6:
            raise ValueError('Version block nesting/count/bounds invalid.')
        length, value_length, kind = struct.unpack_from('<HHH', resource, start)
        end, position = start + length, start + 6
        if length < 8 or end > limit or kind not in [0, 1]:
            raise ValueError('Invalid version block length or value type.')
        key_start = position
        while position + 2 <= end and resource[position:position + 2] != b'\0\0':
            position += 2
            if position - key_start > 256:
                raise ValueError('Version key exceeds its bound.')
        if position + 2 > end:
            raise ValueError('Version key is not terminated.')
        try:
            key = resource[key_start:position].decode('utf-16le')
        except UnicodeError:
            raise ValueError('Version key is not valid UTF-16.') from None
        position += 2
        value_start = align(position)
        if value_start > end:
            # A block with no value/children can end immediately after its key.
            if value_length or position != end:
                raise ValueError('Version value alignment exceeds its block.')
            value_start = position
        zeros(position, value_start)
        value_end = value_start + value_length * (2 if kind == 1 else 1)
        if value_end > end:
            raise ValueError('Version value exceeds its block.')
        value, children = resource[value_start:value_end], {}
        position = value_end
        while position < end:
            aligned = min(align(position), end)
            zeros(position, aligned)
            position = aligned
            if position == end:
                break
            name, child_kind, child_value, descendants, child_end = block(position, end, depth + 1)
            if name in children:
                raise ValueError('Duplicate version block key.')
            children[name] = (child_kind, child_value, descendants)
            position = child_end
        return key, kind, value, children, end

    key, kind, value, children, end = block(0, len(resource))
    if key != 'VS_VERSION_INFO' or kind != 0 or len(value) != 52 or len(resource) - end > 3:
        raise ValueError('Missing exact fixed version-information root.')
    zeros(end, len(resource))
    fixed = struct.unpack('<13I', value)
    major, minor, patch, revision = expected
    words = [(major << 16) | minor, (patch << 16) | revision]
    pre = '-' in version.split('+')[0]
    if (fixed[:2] != (0xfeef04bd, 0x10000) or list(fixed[2:6]) != words * 2
            or fixed[6] != 0x3f or fixed[7] not in [2 if pre else 0, 3 if pre else 1]
            or fixed[8:] != (0x40004, 1, 0, 0, 0) or set(children) != {'StringFileInfo', 'VarFileInfo'}):
        raise ValueError('Fixed application version, flags, type or children differ from source.')
    string_info = children['StringFileInfo']
    variable_info = children['VarFileInfo']
    if (string_info[:2] != (1, b'') or set(string_info[2]) != {'040904B0'}
            or variable_info[:2] != (1, b'') or set(variable_info[2]) != {'Translation'}
            or variable_info[2]['Translation'] != (0, struct.pack('<HH', 0x409, 1200), {})):
        raise ValueError('Version translation/container contract differs.')
    table = string_info[2]['040904B0']
    expected_strings = strings(version)
    if table[:2] != (1, b'') or set(table[2]) != set(expected_strings):
        raise ValueError('Version application metadata fields differ.')
    for name, expected_value in expected_strings.items():
        if table[2][name] != (1, (expected_value + '\0').encode('utf-16le'), {}):
            raise ValueError('Standard version/application/license text differs from source.')
    return dict(format='PeerBrushWindowsVersionResourceV1', version=version, numeric_version=expected,
                debug=bool(fixed[7] & 1), prerelease=pre, language=0x409, codepage=1200)


def check(data, version, sources, manifest):
    present = manifest.get('windows_version_resource')
    needs_resource = required(sources)
    if (needs_resource != (type(present) is int and present == 1)
            or (present is not None and (type(present) is not int or present != 1))):
        raise ValueError('Windows resource layout must match the exact committed source capability.')
    return inspect(data, version) if needs_resource else None
