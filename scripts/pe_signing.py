"""Narrow x64 PE signing-only byte equivalence; never establishes signature trust."""
import struct

CERTIFICATE_LIMIT = 4 * 1024 * 1024


def layout(data):
    # PE32+ offsets follow the Microsoft PE format, not an Authenticode image hash.
    # Every original byte, including overlay/debug data, is compared separately.
    if type(data) is not bytes or len(data) < 64 or data[:2] != b'MZ':
        raise ValueError('Expected an x64 PE32+ executable.')
    pe = struct.unpack_from('<I', data, 60)[0]
    if pe < 64 or pe > len(data) - 24 or data[pe:pe + 4] != b'PE\0\0':
        raise ValueError('Invalid PE header bounds.')
    machine, sections = struct.unpack_from('<HH', data, pe + 4)
    optional_size, characteristics = struct.unpack_from('<HH', data, pe + 20)
    optional = pe + 24
    if (machine != 0x8664 or not 1 <= sections <= 96 or not characteristics & 2
            or not 152 <= optional_size <= 4096 or optional_size > len(data) - optional
            or struct.unpack_from('<H', data, optional)[0] != 0x20b):
        raise ValueError('Unsupported or malformed PE32+ header.')
    directories = struct.unpack_from('<I', data, optional + 108)[0]
    header_size = struct.unpack_from('<I', data, optional + 60)[0]
    section_table = optional + optional_size
    if (not 5 <= directories <= 16 or 112 + 8 * directories > optional_size
            or not section_table + 40 * sections <= header_size <= len(data)):
        raise ValueError('Invalid PE directory or section-table bounds.')
    ranges = []
    for index in range(sections):
        size, pointer = struct.unpack_from('<II', data, section_table + 40 * index + 16)
        if size:
            if pointer < header_size or pointer > len(data) or size > len(data) - pointer:
                raise ValueError('Invalid PE raw section bounds.')
            ranges.append((pointer, pointer + size))
    ranges.sort()
    if any(left[1] > right[0] for left, right in zip(ranges, ranges[1:])):
        raise ValueError('Overlapping PE raw sections.')
    security = optional + 112 + 4 * 8
    address, size = struct.unpack_from('<II', data, security)
    return optional + 64, security, address, size, max([header_size, *[end for _, end in ranges]])


def unsigned(data):
    checksum, security, address, size, _ = layout(data)
    if address or size:
        raise ValueError('The source-bound PE must have no certificate table.')
    return checksum, security


def certificate(data):
    _, _, address, size, raw_end = layout(data)
    if (address % 8 or address < raw_end or not 8 < size <= CERTIFICATE_LIMIT
            or address > len(data) or size != len(data) - address):
        raise ValueError('PE signature must be one bounded certificate table at EOF.')
    length, revision, kind = struct.unpack_from('<IHH', data, address)
    if not 8 < length <= size or ((length + 7) & ~7) != size or revision != 0x200 or kind != 2:
        raise ValueError('PE signature must contain one WIN_CERTIFICATE PKCS7 entry.')
    start = address + 8
    if data[start] != 0x30:
        raise ValueError('PE certificate must contain one DER sequence.')
    first = data[start + 1]
    if first < 128:
        header, content = 2, first
    else:
        count = first & 0x7f
        if not 1 <= count <= 4 or 2 + count > length - 8 or data[start + 2] == 0:
            raise ValueError('PE certificate has invalid DER length.')
        header, content = 2 + count, int.from_bytes(data[start + 2:start + 2 + count], 'big')
        if content < 128 or content.bit_length() <= 8 * (count - 1):
            raise ValueError('PE certificate DER length is not canonical.')
    end = start + header + content
    if (not content or end > address + length or address + size - end > 7
            or any(data[end:address + size])):
        raise ValueError('PE certificate contains trailing or malformed unreviewed data.')
    return data[start:end]


def signing_only(before, after):
    checksum, security = unsigned(before)
    after_checksum, after_security, address, _, _ = layout(after)
    certificate(after)
    if (checksum != after_checksum or security != after_security
            or address != ((len(before) + 7) & ~7) or any(after[len(before):address])
            or before[:checksum] != after[:checksum]
            or before[checksum + 4:security] != after[checksum + 4:security]
            or before[security + 8:] != after[security + 8:len(before)]):
        raise ValueError('PE signing changed bytes outside checksum, certificate directory and append.')
