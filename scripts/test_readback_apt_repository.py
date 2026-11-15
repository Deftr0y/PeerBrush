"""Transport regressions with synthetic endpoints and no DNS or network calls."""
import email.message
import io
import socket
import unittest
from unittest import mock

import readback_apt_repository as public
from prepare_site_release import sha


class PublicAptReadback(unittest.TestCase):
    def setUp(self):
        self.origin = public.repository_origin('https://packages.example.invalid/peerbrush/apt/')
        self.data = b'fixture public bytes'
        self.record = dict(bytes=len(self.data), sha256=sha(self.data))
        self.path = 'dists/noble/InRelease'

    def compare(self, *, status=200, data=None, headers=()):
        response = mock.MagicMock(status=status)
        response.__enter__.return_value = response
        response.headers = email.message.Message()
        for name, value in headers:
            response.headers[name] = value
        response.read1.side_effect = io.BytesIO(self.data if data is None else data).read
        with mock.patch.object(public, 'connect', return_value=mock.sentinel.tls) as connect, \
                mock.patch.object(public.http.client, 'HTTPSConnection') as factory:
            connection = factory.return_value
            connection.getresponse.return_value = response
            try:
                public.compare(self.origin, [mock.sentinel.address], mock.sentinel.context, self.path, self.record)
            finally:
                connection.close.assert_called_once_with()
            self.assertEqual(connect.call_args.args[:3], (self.origin.hostname, [mock.sentinel.address], mock.sentinel.context))
            self.assertEqual(connection.sock, mock.sentinel.tls)
            self.assertEqual(connection.request.call_args.args, ('GET', '/peerbrush/apt/' + self.path))
            request_headers = connection.request.call_args.kwargs['headers']
            self.assertEqual(request_headers['Accept-Encoding'], 'identity')
            self.assertNotIn('Authorization', request_headers)
            self.assertNotIn('Cookie', request_headers)

    def test_requires_full_exact_bytes_at_the_direct_url(self):
        self.compare(headers=[('Content-Length', str(len(self.data)))])
        self.compare(headers=[('Transfer-Encoding', 'chunked')])
        for changes in [dict(status=302, headers=[('Location', 'https://other.example.invalid/object')]),
                        dict(status=206), dict(data=self.data[:-1]), dict(data=self.data + b'x'),
                        dict(data=b'x' * len(self.data)), dict(headers=[('Content-Encoding', 'gzip')]),
                        dict(headers=[('Content-Length', '1'), ('Content-Length', '1')]),
                        dict(headers=[('Transfer-Encoding', 'chunked'), ('Content-Length', str(len(self.data)))])]:
            with self.subTest(changes=changes), self.assertRaises(ValueError):
                self.compare(**changes)

    def test_rejects_private_mapped_multicast_and_mixed_dns_answers(self):
        def answer(family, endpoint):
            return family, socket.SOCK_STREAM, socket.IPPROTO_TCP, '', endpoint
        global_address = answer(socket.AF_INET, ('1.1.1.1', 443))
        with mock.patch.object(public.socket, 'getaddrinfo', return_value=[global_address]):
            self.assertEqual(public.addresses(self.origin.hostname), [global_address[:3] + (global_address[-1],)])
        for blocked in [answer(socket.AF_INET, ('127.0.0.1', 443)), answer(socket.AF_INET, ('224.0.0.1', 443)),
                        answer(socket.AF_INET6, ('::ffff:192.168.1.1', 443, 0, 0))]:
            with self.subTest(blocked=blocked), mock.patch.object(public.socket, 'getaddrinfo',
                    return_value=[global_address, blocked]), self.assertRaises(ValueError):
                public.addresses(self.origin.hostname)


if __name__ == '__main__':
    unittest.main()
