/** @type {import('tailwindcss').Config} */
module.exports = {
  content: ["./**/*.html"],
  darkMode: 'class',
  theme: {
    extend: {
      fontFamily: {
        sans: ['Inter', 'sans-serif'],
        mono: ['Fira Code', 'monospace'],
      },
      colors: {
        // Dark Mode Theme Tokens
        dark: {
          base: '#16171D',
          panel: '#1C1E26',
          subtle: '#242734',
          border: '#2E3244',
          text: '#C0CAF5',
          muted: '#565F89',
          accent: '#37505C',
          violet: '#BB9AF7',
        },
        // Light Mode Theme Tokens
        light: {
          base: '#F4F5F8',
          panel: '#FFFFFF',
          subtle: '#EAECEF',
          border: '#D5D8E1',
          text: '#1E202B',
          muted: '#6C7393',
          accent: '#4C6E7E',
          violet: '#7C4DFF',
        },
        // Database Engine Specific Tokens
        db: {
          mysqlLight: '#4479A1',
          mysql: '#699fc9',
          postgresLight: '#4169E1',
          postgres: '#5884fe',
          sqliteLight: '#003B57',
          sqlite: '#5D89A6',
        }
      },
      animation: {
        'pulse-slow': 'pulse 3s cubic-bezier(0.4, 0, 0.6, 1) infinite',
        'float': 'float 6s ease-in-out infinite',
      },
      keyframes: {
        float: {
          '0%, 100%': { transform: 'translateY(0px)' },
          '50%': { transform: 'translateY(-8px)' },
        }
      }
    },
  },
  plugins: [],
}
