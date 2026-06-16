import 'package:flutter/material.dart';
import 'domain/contact.dart';

/// Entry point for the application.
void main() {
  runApp(const MyApp());
}

/// Root widget of the application.
class MyApp extends StatelessWidget {
  const MyApp({super.key});

  @override
  Widget build(BuildContext context) {
    return const MaterialApp(
      title: 'Contacts',
      home: Scaffold(body: Center(child: Text('Contacts App'))),
    );
  }
}
